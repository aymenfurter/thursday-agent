import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { readFile, mkdir, writeFile, rename, unlink } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { randomUUID } from "node:crypto";

const exec = promisify(execFile);

export function configPath(env = process.env, platform = process.platform) {
    return env.THURSDAY_CONFIG || join(
        platform === "darwin" ? join(homedir(), "Library", "Application Support")
            : env.XDG_CONFIG_HOME || join(homedir(), ".config"),
        "thursday", "config.json",
    );
}

export async function readJson(path, fallback) {
    try {
        return JSON.parse(await readFile(path, "utf8"));
    } catch (error) {
        if (error.code === "ENOENT") return fallback;
        throw new Error(`Cannot read ${path}: ${error.message}`);
    }
}

export async function writeJson(path, value) {
    await mkdir(dirname(path), { recursive: true, mode: 0o700 });
    const temporary = `${path}.${randomUUID()}.tmp`;
    try {
        await writeFile(temporary, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600, flag: "wx" });
        await rename(temporary, path);
    } finally {
        await unlink(temporary).catch(error => { if (error.code !== "ENOENT") throw error; });
    }
}

export async function az(args) {
    try {
        const { stdout } = await exec("az", [...args, "--output", "json", "--only-show-errors"], {
            timeout: 45_000, maxBuffer: 16 * 1024 * 1024,
        });
        return JSON.parse(stdout);
    } catch (error) {
        const detail = error.code === "ENOENT" ? "Azure CLI is not installed."
            : error.killed ? "Azure CLI timed out."
                : String(error.stderr || error.message).trim().slice(0, 1500);
        throw new Error(`Azure discovery failed. ${detail} Sign in with az login and check the subscription.`);
    }
}

export async function discoverAccounts(runAz = az) {
    const subscriptions = await runAz(["account", "list", "--all", "--query",
        "[].{id:id,name:name,tenantId:tenantId,tenantDisplayName:tenantDisplayName,state:state,isDefault:isDefault}"]);
    const tenants = new Map();
    const errors = [];
    const tenantArgs = ["rest", "--method", "get", "--url", "https://management.azure.com/tenants?api-version=2022-12-01",
        "--query", "value[].{id:tenantId,name:displayName,domain:defaultDomain}"];
    try {
        for (const tenant of await runAz(tenantArgs)) tenants.set(tenant.id, tenant);
    } catch (error) {
        errors.push(`Tenant names could not be loaded: ${error.message}`);
    }
    for (const subscription of subscriptions) {
        if (!tenants.has(subscription.tenantId)) {
            try {
                for (const tenant of await runAz([...tenantArgs, "--subscription", subscription.id])) tenants.set(tenant.id, tenant);
            } catch (error) {
                errors.push(`Tenant name lookup for ${subscription.name}: ${error.message}`);
            }
            if (!tenants.has(subscription.tenantId)) tenants.set(subscription.tenantId, {
                id: subscription.tenantId,
                name: subscription.tenantDisplayName || `Signed-in tenant (${subscription.tenantId.slice(0, 8)})`,
            });
        }
    }
    return {
        subscriptions: subscriptions.sort((a, b) => a.name.localeCompare(b.name)),
        tenants: [...tenants.values()].map(tenant => ({
            ...tenant, name: tenant.name || tenant.domain || `Signed-in tenant (${tenant.id.slice(0, 8)})`,
            subscriptionCount: subscriptions.filter(item => item.tenantId === tenant.id).length,
        })).sort((a, b) => a.name.localeCompare(b.name)),
        errors,
    };
}

export function isLiveModel(name) {
    return /^gpt-live-\d+(?:[.-]\d+)*(?:-|$)/.test(name || "");
}

export function resourceEndpoint(account) {
    const properties = account.properties || {};
    const endpoints = properties.endpoints || {};
    return endpoints["AI Foundry API"] || endpoints["OpenAI Realtime Live WebSocket API"] || properties.endpoint;
}

export async function discover(subscription, { runAz = az, env = process.env, fetchModels = fetch } = {}) {
    if (subscription && !/^[a-zA-Z0-9][a-zA-Z0-9 ._()-]{0,199}$/.test(subscription)) {
        throw new Error("Enter a valid Azure subscription name or ID.");
    }
    const errors = [];
    let account;
    let resources = [];
    try {
        account = await runAz(["account", "show", ...(subscription ? ["--subscription", subscription] : []),
            "--query", "{id:id,name:name,tenantId:tenantId}"]);
        const accounts = await runAz(["cognitiveservices", "account", "list", "--subscription", account.id]);
        const foundry = accounts.filter(item => ["AIServices", "OpenAI"].includes(item.kind));
        for (let offset = 0; offset < foundry.length; offset += 3) {
            const batch = await Promise.all(foundry.slice(offset, offset + 3).map(async item => {
                const resourceGroup = item.resourceGroup || item.id.split("/")[4];
                const resource = {
                    id: item.id, name: item.name, location: item.location,
                    endpoint: resourceEndpoint(item), deployments: [],
                };
                try {
                    const deployments = await runAz(["cognitiveservices", "account", "deployment", "list",
                        "--subscription", account.id, "--resource-group", resourceGroup, "--name", item.name]);
                    resource.deployments = deployments.map(deployment => ({
                        name: deployment.name, model: deployment.properties?.model?.name,
                        state: deployment.properties?.provisioningState,
                        compatible: Boolean(resource.endpoint)
                            && deployment.properties?.provisioningState === "Succeeded"
                            && isLiveModel(deployment.properties?.model?.name),
                    }));
                } catch (error) {
                    resource.error = error.message;
                    errors.push(`${item.name}: ${error.message}`);
                }
                return resource;
            }));
            resources.push(...batch);
        }
    } catch (error) {
        errors.push(error.message);
    }
    resources.sort((a, b) => Number(b.deployments.some(d => d.compatible))
        - Number(a.deployments.some(d => d.compatible)) || a.name.localeCompare(b.name));
    const openai = { available: Boolean(env.OPENAI_API_KEY?.trim()), models: [] };
    if (openai.available) {
        try {
            const response = await fetchModels("https://api.openai.com/v1/models", {
                headers: { Authorization: `Bearer ${env.OPENAI_API_KEY}` },
                signal: AbortSignal.timeout(20_000), redirect: "error",
            });
            if (!response.ok) throw new Error(`OpenAI model discovery returned HTTP ${response.status}.`);
            const body = await response.json();
            if (!Array.isArray(body.data)) throw new Error("OpenAI returned an invalid model list.");
            openai.models = body.data.map(item => item.id).filter(isLiveModel).sort();
            if (!openai.models.length) errors.push("This OpenAI key has no GPT-Live models. Check model access.");
        } catch (error) {
            errors.push(error.message.replaceAll(env.OPENAI_API_KEY, "[redacted]"));
        }
    }
    return { subscription: account || null, resources, openai, errors };
}

export function selectionFor(catalog, choice) {
    if (!choice || !["azure", "openai"].includes(choice.provider)) throw new Error("Choose a provider.");
    if (choice.provider === "openai") {
        if (!catalog.openai.available || !catalog.openai.models.includes(choice.model)) {
            throw new Error("Choose an available OpenAI GPT-Live model.");
        }
        return { provider: "openai", model: choice.model };
    }
    const resource = catalog.resources.find(item => item.id === choice.resourceId);
    const deployment = resource?.deployments.find(item => item.name === choice.model && item.compatible);
    if (!deployment || !catalog.subscription) throw new Error("Choose a deployed GPT-Live model on an Azure resource.");
    return {
        provider: "azure", endpoint: resource.endpoint, model: deployment.name,
        subscription: catalog.subscription.id, tenant: catalog.subscription.tenantId,
    };
}

export function preferredChoice(catalog, saved, env = process.env) {
    if (saved?.provider === "openai" && catalog.openai.models.includes(saved.model)) return saved;
    const resource = catalog.resources.find(item => item.endpoint?.replace(/\/$/, "")
        === (saved?.endpoint || env.AZURE_OPENAI_ENDPOINT)?.replace(/\/$/, "")
        && item.deployments.some(deployment => deployment.compatible))
        || catalog.resources.find(item => item.deployments.some(deployment => deployment.compatible));
    if (resource) return {
        provider: "azure", resourceId: resource.id,
        model: resource.deployments.find(item => item.compatible && item.name === saved?.model)?.name
            || resource.deployments.find(item => item.compatible).name,
    };
    return catalog.openai.models[0] ? { provider: "openai", model: catalog.openai.models[0] } : null;
}
