import test from "node:test";
import assert from "node:assert/strict";
import { stat, rm, mkdir, readFile, writeFile, utimes } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { EventEmitter } from "node:events";
import { join } from "node:path";
import { request as httpRequest } from "node:http";
import { fileURLToPath } from "node:url";
import { stripVTControlCharacters } from "node:util";
import { runInNewContext } from "node:vm";
import { discover, discoverAccounts, preferredChoice, selectionFor, isLiveModel, readJson, writeJson } from "./catalog.mjs";
import { PromptBridge } from "./bridge.mjs";
import { startServer } from "./server.mjs";
import { checkLocalPermissions, permissionCommand, processContext } from "./permissions.mjs";
import { ensureCurrentBinary, BUILD_COMMAND } from "./build.mjs";

async function fixtureDirectory(t) {
    const path = join(".github", "extensions", "thursday", `.test-fixture-${randomUUID()}`);
    await mkdir(path);
    t.after(() => rm(path, { recursive: true }));
    return path;
}

test("canvas refuses a missing or stale release without executing it", async t => {
    const root = await fixtureDirectory(t);
    const binary = join(root, "thursday-agent");
    await assert.rejects(ensureCurrentBinary(root, binary), /Build thursday-agent first/);
    await mkdir(join(root, "src", "copilot"), { recursive: true });
    const sources = ["Cargo.toml", "Cargo.lock", "src/main.rs", "src/copilot/mod.rs"];
    for (const path of [...sources, "thursday-agent"]) {
        await writeFile(join(root, path), "fixture");
        await utimes(join(root, path), 100, 100);
    }
    await utimes(binary, 200, 200);
    await ensureCurrentBinary(root, binary);
    for (const path of sources) {
        await utimes(join(root, path), 300, 300);
        await assert.rejects(ensureCurrentBinary(root, binary), error =>
            error.message.includes(path) && error.message.includes(BUILD_COMMAND));
        await utimes(join(root, path), 100, 100);
    }
    await writeFile(join(root, "README.md"), "A documentation edit does not require a rebuild.");
    await ensureCurrentBinary(root, binary);
});

const permissionNames = ["Screen Recording", "Accessibility", "Event Synthesizing"];
const permissionPayload = (grants = [true, false, false], source = "local") => ({
    success: true,
    data: { source, permissions: permissionNames.map((name, index) => ({ name, isGranted: grants[index] })) },
});
function permissionExec(result = permissionPayload(), calls = []) {
    return async (program, args, options) => {
        calls.push({ program, args, options });
        if (program === "/bin/ps") {
            const pid = args[args.indexOf("-p") + 1];
            return { stdout: pid === "81" ? "  81 60 /usr/local/bin/node\n"
                : " 60 1 /Applications/Example Host.app/Contents/MacOS/example-host\n" };
        }
        if (result instanceof Error) throw result;
        return { stdout: typeof result === "string" ? result : JSON.stringify(result) };
    };
}
const permissionOptions = { pid: 81, platform: "darwin" };

test("permissions identify the current process and exact app path before checking locally", async () => {
    const calls = [];
    const report = await checkLocalPermissions({ ...permissionOptions, exec: permissionExec(undefined, calls) });
    assert.equal(report.context.current.pid, 81);
    assert.equal(report.context.hostApp.pid, 60);
    assert.equal(report.context.hostApp.name, "Example Host");
    assert.equal(report.context.hostApp.path, "/Applications/Example Host.app");
    assert.deepEqual(calls.map(call => call.program), ["/bin/ps", "/bin/ps", "npx"]);
    assert.deepEqual(calls.at(-1).args, [
        "--offline", "--yes=false", "@steipete/peekaboo", "permissions", "--json", "--no-remote",
    ]);
    assert.equal(calls.at(-1).options.timeout, 20_000);
    assert.equal(report.status, "denied");
    assert.deepEqual(report.permissions.map(item => item.granted), [true, false, false]);
    assert.ok(Number.isFinite(Date.parse(report.checkedAt)));
});

test("all grants are required for the all-granted summary", async () => {
    const report = await checkLocalPermissions({
        ...permissionOptions, exec: permissionExec(permissionPayload([true, true, true])),
    });
    assert.equal(report.status, "granted");
    assert.equal(report.error, null);
});

test("a standalone process has no guessed host app", async () => {
    const context = await processContext(81, async () => ({ stdout: "81 1 /usr/local/bin/node\n" }));
    assert.equal(context.hostApp, null);
    assert.equal(context.ancestors.length, 1);
});

test("process failures and invalid identities prevent the permission probe", async () => {
    for (const output of ["", "82 1 /usr/bin/node", "81 81 /usr/bin/node", new Error("process exited")]) {
        const report = await checkLocalPermissions({
            ...permissionOptions,
            exec: async program => {
                assert.equal(program, "/bin/ps");
                if (output instanceof Error) throw output;
                return { stdout: output };
            },
        });
        assert.equal(report.status, "error");
        assert.ok(report.error);
        assert.equal(report.probe, null);
        assert.ok(report.permissions.every(item => item.granted === null));
    }
});

test("the configured controller is preserved and invalid commands are rejected", () => {
    assert.deepEqual(permissionCommand(["/Applications/Local Tools/peekaboo"]), {
        program: "/Applications/Local Tools/peekaboo", args: ["permissions", "--json", "--no-remote"],
    });
    for (const command of [[], null, "peekaboo", [""], ["peekaboo", 1]]) {
        assert.throws(() => permissionCommand(command), /command is invalid/);
    }
});

test("missing or malformed grants stay unknown, never denied or granted by default", async () => {
    for (const grants of [[true], [true, "true", false]]) {
        const report = await checkLocalPermissions({
            ...permissionOptions, exec: permissionExec(permissionPayload(grants)),
        });
        assert.equal(report.status, "unknown");
        assert.match(report.error, /did not report/);
        assert.equal(report.permissions[1].granted, null);
    }
});

test("malformed, failed and remote permission reports cannot claim local grants", async () => {
    for (const payload of ["not JSON", "null", { success: false }, permissionPayload([true, true, true], "remote")]) {
        const report = await checkLocalPermissions({ ...permissionOptions, exec: permissionExec(payload) });
        assert.equal(report.status, "error");
        assert.ok(report.error);
        assert.ok(report.permissions.every(item => item.granted === null));
        assert.equal(report.context.current.pid, 81);
    }
});

test("missing executables, failures and timeouts are explicit without exposing command output", async () => {
    for (const [properties, message] of [
        [{ code: "ENOENT" }, /Cannot find/],
        [{ killed: true }, /timed out/],
        [{ code: 1 }, /command failed/],
    ]) {
        const report = await checkLocalPermissions({
            ...permissionOptions, exec: permissionExec(Object.assign(new Error("private command output"), properties)),
        });
        assert.equal(report.status, "error");
        assert.match(report.error, message);
        assert.ok(!JSON.stringify(report).includes("private command output"));
        assert.ok(report.permissions.every(item => item.granted === null));
    }
});

test("non-macOS hosts report unsupported without starting a command", async () => {
    const report = await checkLocalPermissions({ platform: "linux", exec: () => assert.fail("unexpected command") });
    assert.equal(report.status, "error");
    assert.match(report.error, /require macOS/);
});

test("permission refresh is canvas-only and returns denied states without hiding them", async t => {
    const expected = await checkLocalPermissions({ ...permissionOptions, exec: permissionExec() });
    let checks = 0;
    const app = await startServer({ permissions: async () => { checks++; return expected; } });
    t.after(() => app.server.close());
    const headers = { "Content-Type": "application/json", Authorization: `Bearer ${app.token}` };
    assert.equal((await fetch(`${app.url}permissions`, { method: "POST", headers, body: "{}" })).status, 403);
    assert.equal(checks, 0);
    const response = await fetch(`${app.canvasUrl}permissions`, { method: "POST", headers, body: "{}" });
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), expected);
    assert.equal(checks, 1);
});

const account = { id: "subscription-1", name: "Test", tenantId: "tenant-1" };
const resource = {
    id: "/subscriptions/subscription-1/resourceGroups/group/providers/Microsoft.CognitiveServices/accounts/foundry",
    name: "foundry", kind: "AIServices", location: "swedencentral",
    properties: { endpoints: { "AI Foundry API": "https://foundry.example/" } },
};
const deployment = (name, model, state = "Succeeded") => ({ name, properties: { model: { name: model }, provisioningState: state } });
function azure(args) {
    if (args[0] === "account") return Promise.resolve(account);
    if (args.includes("deployment")) return Promise.resolve([
        deployment("voice-production", "gpt-live-1"),
        deployment("chat", "gpt-5.4"), deployment("pending", "gpt-live-1", "Creating"),
    ]);
    return Promise.resolve([resource, { ...resource, id: "speech", kind: "SpeechServices" }]);
}
const models = async () => ({ ok: true, json: async () => ({ data: [{ id: "gpt-live-1" }, { id: "gpt-5.4" }] }) });
const both = () => discover("subscription-1", { runAz: azure, env: { OPENAI_API_KEY: "test-secret" }, fetchModels: models });

test("account dropdown includes every cached subscription and tenant names", async () => {
    const accounts = await discoverAccounts(async args => args[0] === "account" ? [
        { id: "b", name: "Production", tenantId: "tenant-b", state: "Enabled" },
        { id: "a", name: "Development", tenantId: "tenant-a", state: "Enabled", isDefault: true },
        { id: "c", name: "Archive", tenantId: "tenant-a", state: "Disabled" },
    ] : [
        { id: "tenant-a", name: "Company", domain: "company.example" },
        { id: "tenant-b", name: "Lab", domain: "lab.example" },
        { id: "tenant-c", name: "Guest", domain: "guest.example" },
    ]);
    assert.equal(accounts.subscriptions.length, 3);
    assert.equal(accounts.tenants.length, 3);
    assert.equal(accounts.tenants.find(item => item.id === "tenant-a").subscriptionCount, 2);
    assert.equal(accounts.tenants.find(item => item.id === "tenant-c").subscriptionCount, 0);
    assert.deepEqual(accounts.subscriptions.map(item => item.name), ["Archive", "Development", "Production"]);
});

test("tenant lookup failure keeps cached subscriptions and displays an error", async () => {
    const accounts = await discoverAccounts(async args => {
        if (args[0] === "account") return [{ id: "a", name: "Development", tenantId: "tenant-a", tenantDisplayName: "Cached company" }];
        throw new Error("tenant lookup denied");
    });
    assert.equal(accounts.subscriptions.length, 1);
    assert.equal(accounts.tenants[0].name, "Cached company");
    assert.match(accounts.errors[0], /tenant lookup denied/);
});

test("discovery lists Foundry deployments and only enables deployed live models", async () => {
    const catalog = await both();
    assert.equal(catalog.resources.length, 1);
    assert.deepEqual(catalog.resources[0].deployments.map(item => item.compatible), [true, false, false]);
    assert.deepEqual(catalog.openai.models, ["gpt-live-1"]);
    assert.equal(catalog.resources[0].endpoint, resource.properties.endpoints["AI Foundry API"]);
});

test("Azure is preferred when both providers are available", async () => {
    assert.equal(preferredChoice(await both(), null, {}).provider, "azure");
});

test("an explicit saved OpenAI selection is preserved", async () => {
    const saved = { provider: "openai", model: "gpt-live-1" };
    assert.deepEqual(preferredChoice(await both(), saved), saved);
});

test("selection uses the Azure deployment name rather than its model family", async () => {
    assert.deepEqual(selectionFor(await both(), { provider: "azure", resourceId: resource.id, model: "voice-production" }), {
        provider: "azure", endpoint: "https://foundry.example/", model: "voice-production",
        subscription: "subscription-1", tenant: "tenant-1",
    });
});

test("undeployed, non-live and unlisted choices are rejected", async () => {
    const catalog = await both();
    for (const model of ["chat", "pending", "fake"]) {
        assert.throws(() => selectionFor(catalog, { provider: "azure", resourceId: resource.id, model }));
    }
    assert.throws(() => selectionFor(catalog, { provider: "openai", model: "gpt-5.4" }));
    assert.throws(() => selectionFor(catalog, { provider: "unknown" }));
    assert.equal(isLiveModel("gpt-realtime"), false);
    assert.equal(isLiveModel("gpt-live-transcribe"), false);
});

test("OpenAI is not requested or exposed without an environment key", async () => {
    const catalog = await discover("", { runAz: azure, env: {}, fetchModels: () => assert.fail("unexpected OpenAI request") });
    assert.equal(catalog.openai.available, false);
    assert.throws(() => selectionFor(catalog, { provider: "openai", model: "gpt-live-1" }));
});

test("Azure errors are visible and do not hide a configured OpenAI provider", async () => {
    const catalog = await discover("", {
        runAz: async () => { throw new Error("az login required"); },
        env: { OPENAI_API_KEY: "test-secret" }, fetchModels: models,
    });
    assert.deepEqual(catalog.errors, ["az login required"]);
    assert.equal(preferredChoice(catalog, null, {}).provider, "openai");
});

test("deployment errors remain visible for the affected resource", async () => {
    const catalog = await discover("", {
        runAz: args => args.includes("deployment") ? Promise.reject(new Error("forbidden")) : azure(args), env: {},
    });
    assert.equal(catalog.resources[0].error, "forbidden");
    assert.equal(catalog.resources[0].deployments.length, 0);
    assert.equal(preferredChoice(catalog, null, {}), null);
});

test("OpenAI discovery errors do not contain the key", async () => {
    const catalog = await discover("", {
        runAz: azure, env: { OPENAI_API_KEY: "test-secret" },
        fetchModels: async () => { throw new Error("failed test-secret"); },
    });
    assert.equal(JSON.stringify(catalog).includes("test-secret"), false);
    assert.match(catalog.errors[0], /redacted/);
});

test("configuration is atomic, private and preserves other fields", async t => {
    const dir = await fixtureDirectory(t);
    const path = join(dir, "config.json");
    await writeJson(path, { user_name: "Test", live_selection: { provider: "openai", model: "gpt-live-1" } });
    const saved = await readJson(path);
    assert.equal(saved.user_name, "Test");
    assert.equal((await stat(path)).mode & 0o777, 0o600);
});

function mockSession() {
    let listener;
    let messages = 0;
    return {
        sessionId: "test-session",
        on: fn => { listener = fn; return () => {}; },
        send: async () => `message-${++messages}`,
        event: event => listener(event),
        rpc: { queue: {
            pendingItems: async () => ({ items: [] }),
            removeAt: async () => ({ removed: false }),
        } },
        log: async message => assert.fail(message),
    };
}

async function extensionFixture(t) {
    const session = { ...mockSession(), workspacePath: "/mock/session" };
    const config = { live_selection: { provider: "openai", model: "gpt-live-1" } };
    const spawned = [];
    const servers = [];
    const executions = [];
    const signals = new Map();
    const control = {
        permissions: async () => ({ status: "granted" }),
        ensureBinary: async () => {},
        writeConfig: async value => { Object.assign(config, value); },
        stopChild: process => queueMicrotask(() => process.emit("exit", 0, "SIGINT")),
        setTimeout,
    };
    let declaration;
    const source = (await readFile(new URL("./main.mjs", import.meta.url), "utf8"))
        .replace(/^import .*;\n/gm, "")
        .replaceAll("import.meta.url", "extensionUrl");
    await runInNewContext(`(async () => {\n${source}\n})()`, {
        extensionUrl: new URL("./main.mjs", import.meta.url).href,
        join, fileURLToPath, URL, structuredClone, stripVTControlCharacters,
        setTimeout: (...args) => control.setTimeout(...args),
        clearTimeout, BUILD_COMMAND, PromptBridge, preferredChoice, selectionFor,
        process: {
            env: { OPENAI_API_KEY: "test-secret", AZURE_OPENAI_API_KEY: "unused-key" },
            on: (name, handler) => signals.set(name, handler), exit() {},
        },
        joinSession: async value => { declaration = value; return session; },
        createCanvas: value => value,
        configPath: () => "/mock/config.json",
        readJson: async (path, fallback) => path === "/mock/config.json" ? structuredClone(config) : fallback,
        writeJson: async (path, value) => {
            if (path === "/mock/config.json") await control.writeConfig(value);
        },
        discover: both,
        discoverAccounts: async () => ({ subscriptions: [], tenants: [], errors: [] }),
        checkLocalPermissions: () => control.permissions(),
        ensureCurrentBinary: () => control.ensureBinary(),
        promisify: fn => fn,
        execFile: async (_binary, args) => { executions.push(args); return { stdout: "ready" }; },
        spawn: (binary, args, options) => {
            const process = new EventEmitter();
            process.invocation = { binary, args, options };
            process.stdout = new EventEmitter();
            process.stderr = new EventEmitter();
            process.stdout.setEncoding = () => process.stdout;
            process.stderr.setEncoding = () => process.stderr;
            process.kill = signal => { control.stopChild(process, signal); return true; };
            spawned.push(process);
            queueMicrotask(() => process.emit("spawn"));
            return process;
        },
        startServer: async () => {
            const entry = {
                url: `http://127.0.0.1:${servers.length + 1}/`,
                canvasUrl: `http://127.0.0.1:${servers.length + 1}/canvas/test/`,
                token: "test", closed: false,
            };
            entry.server = { close: () => { entry.closed = true; } };
            servers.push(entry);
            return entry;
        },
    });
    const canvas = declaration.canvases[0];
    const action = (name, input) => canvas.actions.find(item => item.name === name).handler({ input });
    t.after(async () => {
        control.stopChild = process => queueMicrotask(() => process.emit("exit", 0, "SIGINT"));
        await action("stop_voice");
    });
    await canvas.open({ instanceId: "first" });
    return { canvas, action, control, config, spawned, servers, executions, signals };
}

test("stopping during startup prevents a worker from starting after the stop", async t => {
    const app = await extensionFixture(t);
    const entered = Promise.withResolvers();
    const permissions = Promise.withResolvers();
    app.control.permissions = () => { entered.resolve(); return permissions.promise; };
    const starting = app.action("start_voice");
    await entered.promise;
    const stopping = app.action("stop_voice");
    permissions.resolve({ status: "granted" });
    await Promise.allSettled([starting, stopping]);
    assert.equal(app.spawned.length, 0);
    assert.equal((await app.action("get_state")).process.status, "stopped");
});

test("startup excludes connection checks and model changes throughout preflight", async t => {
    const app = await extensionFixture(t);
    await app.action("list_resources");
    const entered = Promise.withResolvers();
    const binary = Promise.withResolvers();
    app.control.ensureBinary = () => { entered.resolve(); return binary.promise; };
    const starting = app.action("start_voice");
    await entered.promise;
    const checking = app.action("check_connection");
    const selecting = app.action("select_model", { provider: "azure", resourceId: resource.id, model: "voice-production" });
    binary.resolve();
    const outcomes = await Promise.allSettled([starting, checking, selecting]);
    assert.equal(outcomes[0].status, "fulfilled");
    assert.equal(outcomes[1].status, "rejected", "a connection check must not overlap startup");
    assert.equal(outcomes[2].status, "rejected", "a model save must not overlap startup");
    assert.equal(app.config.live_selection.provider, "openai");
    assert.ok(app.executions.every(args => args.includes("--help")));
    assert.equal(app.spawned.length, 1);
});

test("an in-flight model save excludes startup and connection checks until committed", async t => {
    const app = await extensionFixture(t);
    await app.action("list_resources");
    const entered = Promise.withResolvers();
    const write = Promise.withResolvers();
    app.control.writeConfig = async value => {
        entered.resolve();
        await write.promise;
        Object.assign(app.config, value);
    };
    const selecting = app.action("select_model", { provider: "azure", resourceId: resource.id, model: "voice-production" });
    await entered.promise;
    const starting = app.action("start_voice");
    const checking = app.action("check_connection");
    write.resolve();
    const outcomes = await Promise.allSettled([selecting, starting, checking]);
    assert.equal(outcomes[0].status, "fulfilled");
    assert.equal(outcomes[1].status, "rejected", "startup must not race a model save");
    assert.equal(outcomes[2].status, "rejected", "a connection check must not race a model save");
    assert.equal(app.spawned.length, 0);
});

test("closing the last panel does not close a replacement panel's server while stopping", async t => {
    const app = await extensionFixture(t);
    await app.action("start_voice");
    const stopping = Promise.withResolvers();
    app.control.stopChild = () => stopping.resolve();
    const closing = app.canvas.onClose({ instanceId: "first" });
    await stopping.promise;
    const replacement = await app.canvas.open({ instanceId: "replacement" });
    app.spawned[0].emit("exit", 0, "SIGINT");
    await closing;
    assert.equal(replacement.url, app.servers[0].canvasUrl);
    assert.equal(app.servers[0].closed, false);
    await app.canvas.onClose({ instanceId: "replacement" });
    assert.equal(app.servers[0].closed, true);
});

test("a replacement panel retains its server while its permission check is pending", async t => {
    const app = await extensionFixture(t);
    await app.action("start_voice");
    const stopping = Promise.withResolvers();
    const checking = Promise.withResolvers();
    const permissions = Promise.withResolvers();
    app.control.stopChild = () => stopping.resolve();
    app.control.permissions = () => { checking.resolve(); return permissions.promise; };
    const closing = app.canvas.onClose({ instanceId: "first" });
    await stopping.promise;
    const opening = app.canvas.open({ instanceId: "replacement" });
    await checking.promise;
    app.spawned[0].emit("exit", 0, "SIGINT");
    await closing;
    permissions.resolve({ status: "granted" });
    const replacement = await opening;
    assert.equal(replacement.url, app.servers[0].canvasUrl);
    assert.equal(app.servers[0].closed, false);
    await app.canvas.onClose({ instanceId: "replacement" });
    assert.equal(app.servers[0].closed, true);
});

test("normal starts preserve provider isolation, permission warnings and stop behavior", async t => {
    const app = await extensionFixture(t);
    await app.action("start_voice");
    let launched = app.spawned[0].invocation;
    assert.equal(launched.options.env.OPENAI_API_KEY, "test-secret");
    assert.equal(launched.options.env.AZURE_OPENAI_API_KEY, undefined);
    assert.equal(launched.options.env.THURSDAY_APP_BRIDGE, "/mock/session/files/thursday-bridge.json");
    assert.equal(launched.args[0], "run");
    assert.equal((await app.action("stop_voice")).process.status, "stopped");
    await app.action("list_resources");
    await app.action("select_model", { provider: "azure", resourceId: resource.id, model: "voice-production" });
    app.control.permissions = async () => ({ status: "denied" });
    assert.equal((await app.action("start_voice")).process.status, "running");
    launched = app.spawned[1].invocation;
    assert.equal(launched.options.env.OPENAI_API_KEY, undefined);
    assert.equal(launched.options.env.AZURE_OPENAI_API_KEY, undefined);
    app.spawned[1].stdout.emit("data", "\u001b[31mtest-secret\u001b[0m\n");
    const state = await app.action("get_state");
    assert.match(state.process.log, /voice can still start/);
    assert.match(state.process.log, /\[redacted\]/);
    assert.doesNotMatch(state.process.log, /test-secret|\u001b/);
    await app.canvas.onClose({ instanceId: "first" });
    assert.equal((await app.action("get_state")).process.status, "stopped");
    assert.equal(app.servers[0].closed, true);
});

test("failed preflight and failed model writes release their operation guards", async t => {
    const app = await extensionFixture(t);
    app.control.ensureBinary = async () => { throw new Error("Build required."); };
    await assert.rejects(app.action("start_voice"), /Build required/);
    await app.action("list_resources");
    app.control.writeConfig = async () => { throw new Error("Disk full."); };
    await assert.rejects(app.action("select_model", { provider: "openai", model: "gpt-live-1" }), /Disk full/);
    app.control.ensureBinary = async () => {};
    assert.equal((await app.action("check_connection")).message, "ready");
    assert.equal((await app.action("start_voice")).process.status, "running");
    await assert.rejects(app.action("start_voice"), /already/);
    assert.equal(app.spawned.length, 1);
});

test("extension shutdown escalates a stuck worker before the host's five-second kill deadline", async t => {
    const app = await extensionFixture(t);
    await app.action("start_voice");
    const signals = [];
    let escalationDelay;
    app.control.stopChild = (process, signal) => {
        signals.push(signal);
        if (signal === "SIGKILL") queueMicrotask(() => process.emit("exit", null, signal));
    };
    app.control.setTimeout = (callback, delay) => {
        escalationDelay = delay;
        queueMicrotask(callback);
    };
    await app.signals.get("SIGTERM")();
    assert.ok(escalationDelay > 0 && escalationDelay < 5000, `escalation takes ${escalationDelay}ms`);
    assert.deepEqual(signals, ["SIGINT", "SIGKILL"]);
    assert.equal(app.servers[0].closed, true);
});

test("bridge returns an explicit reply immediately without a final message or idle event", async () => {
    const session = mockSession();
    const bridge = new PromptBridge(session);
    const receipt = await bridge.send("list all sessions");
    assert.equal(receipt.status, "queued");
    session.event({ type: "assistant.message", data: { originatingMessageId: "unrelated", content: "wrong answer" } });
    session.event({ type: "session.idle" });
    assert.equal(receipt.status, "queued");
    session.event({ type: "user.message", data: { messageId: "message-1" } });
    session.event({ type: "session.idle" });
    assert.equal(receipt.status, "running");
    session.event({ type: "assistant.message", data: { originatingMessageId: "message-1", content: "This chat text is not the reply.", toolRequests: [] } });
    session.event({ type: "session.idle" });
    assert.equal(receipt.status, "running");
    assert.equal(receipt.response, undefined);
    const result = bridge.reply(receipt.id, "one session");
    assert.deepEqual(result, { requestId: receipt.id, status: "completed" });
    assert.equal(bridge.get(receipt.id).response, "one session");
    assert.equal(receipt.status, "completed");
    assert.ok(receipt.completedAt);
});

test("every request contains the fixed fast, agent-only reply instructions and its own ID", async () => {
    const session = mockSession();
    const sent = [];
    session.send = async options => { sent.push(options); return `message-${sent.length}`; };
    const bridge = new PromptBridge(session);
    for (const prompt of ["List sessions.", "Get the last activity."]) {
        const receipt = await bridge.send(prompt);
        const message = sent.at(-1);
        assert.match(message.prompt, /Reply as quickly as possible through the thursday_reply tool/);
        assert.match(message.prompt, /Do not narrate, post the answer in chat, render widgets, open canvases/);
        assert.match(message.prompt, /After thursday_reply succeeds, stop without a user-facing message/);
        assert.ok(message.prompt.includes(`requestId: ${receipt.id}`));
        assert.ok(message.prompt.endsWith(prompt));
        assert.equal(message.displayPrompt, "thursday-agent request");
        assert.equal(message.mode, "immediate");
        assert.ok(message.prompt.includes(`expiresAt: ${receipt.expiresAt}`));
        bridge.reply(receipt.id, "Done.");
    }
    assert.equal(sent[0].prompt.split("\n\nrequestId:")[0], sent[1].prompt.split("\n\nrequestId:")[0]);
});

test("replies require an existing request and cannot replace a completed or failed result", async () => {
    const bridge = new PromptBridge(mockSession(), [{ id: "failed", status: "failed", error: "Disconnected." }]);
    assert.throws(() => bridge.reply("missing", "answer"), /not found/);
    assert.throws(() => bridge.reply("failed", "answer"), /failed/);
    const receipt = await bridge.send("List sessions.");
    for (const response of ["", "  ", "x".repeat(16001), null]) {
        assert.throws(() => bridge.reply(receipt.id, response), /plain-text response/);
    }
    const first = bridge.reply(receipt.id, "  one session  ");
    assert.deepEqual(bridge.reply(receipt.id, "one session"), first);
    assert.throws(() => bridge.reply(receipt.id, "different answer"), /cannot be replaced/);
    assert.equal(bridge.get(receipt.id).response, "one session");
});

test("reply before send resolves is not overwritten by subsequent session events", async () => {
    const session = mockSession();
    const bridge = new PromptBridge(session);
    session.send = async options => {
        const id = options.source.slice("thursday-".length);
        bridge.reply(id, "Ready.");
        session.event({ type: "user.message", data: { source: options.source, messageId: "early" } });
        return "early";
    };
    const receipt = await bridge.send("hello");
    session.event({ type: "session.error", data: { message: "an unrelated later failure" } });
    assert.equal(receipt.status, "completed");
    assert.equal(receipt.response, "Ready.");
});

test("bridge handles a user event before send resolves", async () => {
    const session = mockSession();
    session.send = async options => {
        session.event({ type: "user.message", data: { source: options.source, messageId: "early" } });
        return "early";
    };
    const receipt = await new PromptBridge(session).send("hello");
    assert.equal(receipt.status, "running");
    assert.equal(receipt.messageId, "early");
});

test("pending requests do not block new requests or cross their replies", async () => {
    const bridge = new PromptBridge(mockSession());
    await assert.rejects(bridge.send(" "));
    await assert.rejects(bridge.send("x".repeat(16001)));
    const first = await bridge.send("one");
    const second = await bridge.send("two");
    assert.notEqual(first.id, second.id);
    bridge.reply(second.id, "second answer");
    assert.equal(first.status, "queued");
    assert.equal(first.response, undefined);
    assert.equal(second.response, "second answer");
    bridge.reply(first.id, "first answer");
    assert.equal(first.response, "first answer");
});

test("unanswered requests expire and their late replies cannot claim success", async () => {
    let now = 0;
    const session = mockSession();
    const removed = [];
    session.rpc.queue.pendingItems = async () => ({ items: [{ id: "queued-1", messageId: "message-1" }] });
    session.rpc.queue.removeAt = async ({ id }) => { removed.push(id); return { removed: true }; };
    const bridge = new PromptBridge(session, [], () => {}, () => now);
    const old = await bridge.send("old request");
    now = 180_000;
    assert.equal(bridge.get(old.id).status, "failed");
    await bridge.cleanup;
    assert.deepEqual(removed, ["queued-1"]);
    assert.throws(() => bridge.reply(old.id, "too late"), /failed/);
    assert.equal((await bridge.send("new request")).status, "queued");
});

test("restart cleanup removes only messages owned by this bridge", async () => {
    const session = mockSession();
    const removed = [];
    session.rpc.queue.pendingItems = async () => ({ items: [
        { id: "our-queue-item", messageId: "our-message" },
        { id: "user-queue-item", messageId: "user-message" },
    ] });
    session.rpc.queue.removeAt = async ({ id }) => { removed.push(id); return { removed: true }; };
    const bridge = new PromptBridge(session, [{ id: "old", messageId: "our-message", status: "queued" }]);
    await bridge.recover();
    assert.deepEqual(removed, ["our-queue-item"]);
    assert.equal(bridge.get("old").status, "failed");
});

test("history pruning does not discard unanswered requests", async () => {
    const bridge = new PromptBridge(mockSession());
    const first = await bridge.send("first");
    for (let i = 0; i < 55; i++) await bridge.send(`request ${i}`);
    assert.equal(bridge.get(first.id).status, "queued");
    bridge.reply(first.id, "still reachable");
    assert.equal(first.response, "still reachable");
});

test("bridge send errors and session errors return failure", async () => {
    const session = mockSession();
    session.send = async () => { throw new Error("disconnected"); };
    const bridge = new PromptBridge(session);
    assert.equal((await bridge.send("one")).status, "failed");
    session.send = async () => "message-1";
    const receipt = await bridge.send("two");
    session.event({ type: "user.message", data: { messageId: "message-1" } });
    session.event({ type: "session.error", data: { message: "provider unavailable" } });
    assert.equal(receipt.error, "provider unavailable");
});

test("bridge never replays queued work after an extension restart", () => {
    const bridge = new PromptBridge(mockSession(), [{ id: "old", status: "queued", prompt: "send a message" }]);
    assert.equal(bridge.get("old").status, "failed");
    assert.match(bridge.get("old").error, /Check the app conversation/);
});

test("loopback server requires a secret and blocks foreign origins and bad hosts", async t => {
    const app = await startServer({ state: async () => ({ selection: null }) });
    t.after(() => app.server.close());
    assert.equal((await fetch(`${app.url}state`)).status, 401);
    assert.equal((await fetch(`${app.url}state`, { headers: { Authorization: `Bearer ${app.token}`, Origin: "https://evil.example" } })).status, 403);
    const badHost = await new Promise((resolve, reject) => {
        const request = httpRequest(`${app.url}state`, {
            headers: { Authorization: `Bearer ${app.token}`, Host: "evil.example" },
        }, response => { response.resume(); resolve(response.statusCode); });
        request.once("error", reject);
        request.end();
    });
    assert.equal(badHost, 403);
    assert.equal((await fetch(`${app.canvasUrl}state`)).status, 200);
    assert.equal((await fetch(app.canvasUrl)).status, 200);
});

test("canvas serves three accessible tabs, native selectors and local provider marks", async t => {
    const app = await startServer({});
    t.after(() => app.server.close());
    const response = await fetch(app.canvasUrl);
    const html = await response.text();
    assert.match(html, /<title>thursday-agent<\/title>/);
    assert.match(html, /<h1>thursday-agent<\/h1>/);
    assert.match(html, /<img class="brand-logo" src="logo\.svg" width="64" height="64" alt="">/);
    assert.match(html, /<link rel="icon" href="logo\.svg" type="image\/svg\+xml">/);
    assert.doesNotMatch(html, /\bThursday\b/);
    assert.match(response.headers.get("content-security-policy"), /script-src 'self'/);
    assert.equal([...html.matchAll(/role="tab"/g)].length, 3);
    assert.equal([...html.matchAll(/role="tabpanel"/g)].length, 3);
    for (const name of ["voice", "setup", "diagnosis"]) {
        assert.match(html, new RegExp(`id="tab-${name}"[^>]+aria-controls="panel-${name}"`));
        assert.match(html, new RegExp(`id="panel-${name}"[^>]+aria-labelledby="tab-${name}"`));
    }
    for (const name of ["provider", "tenant", "subscription", "resource", "model"]) {
        assert.match(html, new RegExp(`<label for="${name}">`));
        assert.match(html, new RegExp(`<select id="${name}"`));
    }
    assert.match(html, /<symbol id="logo-azure"/);
    assert.match(html, /<symbol id="logo-openai"/);
    assert.doesNotMatch(html, /<(?:script|img|link)\b[^>]+(?:src|href)="https?:/);
    for (const asset of ["ui.js", "style.css"]) {
        assert.equal((await fetch(`${app.canvasUrl}${asset}`)).status, 200);
    }
    const logoResponse = await fetch(`${app.canvasUrl}logo.svg`);
    assert.equal(logoResponse.status, 200);
    assert.match(logoResponse.headers.get("content-type"), /^image\/svg\+xml/);
    const logo = await logoResponse.text();
    assert.match(logo, /<title[^>]*>thursday-agent door logo<\/title>/);
    assert.match(logo, /<path\b/);
    assert.doesNotMatch(logo, /<(?:text|image|script)\b/);
    assert.equal((await fetch(`${app.url}logo.svg`)).status, 401);
    assert.equal((await fetch(`${app.canvasUrl}missing.svg`)).status, 404);
});

test("external bridge is restricted to prompt and receipt operations", async t => {
    const app = await startServer({
        prompt: async prompt => ({ id: "abc", status: "queued", prompt }),
        request: id => ({ id, status: "completed", response: "answer" }),
    });
    t.after(() => app.server.close());
    const headers = { Authorization: `Bearer ${app.token}`, "Content-Type": "application/json" };
    const response = await fetch(`${app.url}prompt`, { method: "POST", headers, body: JSON.stringify({ prompt: "hello" }) });
    assert.equal(response.status, 202);
    assert.equal((await response.json()).status, "queued");
    assert.equal((await fetch(`${app.url}start`, { method: "POST", headers, body: "{}" })).status, 403);
    assert.equal((await (await fetch(`${app.url}requests/abc`, { headers })).json()).response, "answer");
});

test("the HTTP return path delivers an explicit reply while the app session is still busy", async t => {
    const bridge = new PromptBridge(mockSession());
    const app = await startServer({
        prompt: prompt => bridge.send(prompt),
        request: id => bridge.get(id),
    });
    t.after(() => app.server.close());
    const headers = { Authorization: `Bearer ${app.token}`, "Content-Type": "application/json" };
    const receipt = await (await fetch(`${app.url}prompt`, {
        method: "POST", headers, body: JSON.stringify({ prompt: "List sessions." }),
    })).json();
    assert.equal(receipt.status, "queued");
    bridge.reply(receipt.id, "One session.");
    const answer = await (await fetch(`${app.url}requests/${receipt.id}`, { headers })).json();
    assert.equal(answer.status, "completed");
    assert.equal(answer.response, "One session.");
});

test("server rejects non-JSON and oversized submissions", async t => {
    const app = await startServer({});
    t.after(() => app.server.close());
    assert.equal((await fetch(`${app.canvasUrl}prompt`, { method: "POST", body: "hello" })).status, 415);
    const response = await fetch(`${app.canvasUrl}prompt`, {
        method: "POST", headers: { "Content-Type": "application/json" }, body: "x".repeat(21000),
    });
    assert.equal(response.status, 400);
});
