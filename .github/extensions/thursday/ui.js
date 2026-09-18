const byId = id => document.getElementById(id);
let current;
let busy = false;
let pendingAction = "";
let connectionResult = "";
let choicesReady = false;
let accounts;
let permissionSignature = "";
let permissionBusy = false;

const tabs = [...document.querySelectorAll('[role="tab"]')];
function showTab(name, focus = true) {
    for (const tab of tabs) {
        const selected = tab.id === `tab-${name}`;
        tab.setAttribute("aria-selected", String(selected));
        tab.tabIndex = selected ? 0 : -1;
        byId(tab.getAttribute("aria-controls")).hidden = !selected;
        if (selected && focus) tab.focus();
    }
}
tabs.forEach((tab, index) => {
    tab.addEventListener("click", () => showTab(tab.id.slice(4)));
    tab.addEventListener("keydown", event => {
        const next = { ArrowRight: (index + 1) % tabs.length, ArrowLeft: (index + tabs.length - 1) % tabs.length,
            Home: 0, End: tabs.length - 1 }[event.key];
        if (next === undefined) return;
        event.preventDefault();
        showTab(tabs[next].id.slice(4));
    });
});
byId("edit-setup").addEventListener("click", () => showTab("setup"));
byId("open-diagnosis").addEventListener("click", () => showTab("diagnosis"));

function setLogo(id, provider) {
    const logo = byId(id);
    logo.toggleAttribute("hidden", !provider);
    logo.dataset.provider = provider || "";
    logo.querySelector("use").setAttribute("href", `#logo-${provider || "azure"}`);
}

async function request(path, input) {
    const response = await fetch(path, input === undefined ? { cache: "no-store" } : {
        method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(input),
    });
    const value = await response.json();
    if (!response.ok) throw new Error(value.error || `Request failed (${response.status}).`);
    return value;
}

function option(select, value, label, disabled = false) {
    const item = document.createElement("option");
    item.value = value;
    item.textContent = label;
    item.disabled = disabled;
    if (value === "" && disabled) item.selected = true;
    select.append(item);
}

function fillSubscriptions(preferred) {
    const select = byId("subscription");
    const previous = preferred || select.value;
    const query = byId("subscription-filter").value.toLocaleLowerCase().trim();
    const subscriptions = accounts.subscriptions.filter(item => item.tenantId === byId("tenant").value);
    select.replaceChildren();
    for (const subscription of subscriptions.filter(item => item.name.toLocaleLowerCase().includes(query))) {
        const disabled = subscription.state !== "Enabled";
        option(select, subscription.id, subscription.name + (disabled ? ` (${subscription.state})` : ""), disabled);
    }
    const available = [...select.options].filter(item => !item.disabled);
    select.value = available.find(item => item.value === previous)?.value || available[0]?.value || "";
    select.disabled = !available.length;
    if (!select.options.length) option(select, "", query ? "No matching subscriptions" : "No cached subscriptions", true);
    byId("subscription-search").hidden = subscriptions.length <= 8;
    byId("account-help").textContent = !subscriptions.length
        ? "Sign in with az login, then refresh accounts."
        : !available.length ? query ? "No match. Clear the search or use another name." : "No enabled subscriptions in this tenant." : "";
    byId("refresh").disabled = busy || !available.length;
}

async function loadAccounts() {
    accounts = await request("accounts");
    byId("subscription-filter").value = "";
    const preferred = current.selection?.subscription || current.catalog?.subscription?.id
        || accounts.subscriptions.find(item => item.isDefault)?.id;
    const tenantId = accounts.subscriptions.find(item => item.id === preferred)?.tenantId;
    const select = byId("tenant");
    select.replaceChildren();
    for (const tenant of accounts.tenants) {
        const duplicate = accounts.tenants.filter(item => item.name === tenant.name).length > 1;
        const label = `${tenant.name}${duplicate ? ` (${tenant.domain || tenant.id.slice(0, 8)})` : ""}`;
        option(select, tenant.id, label);
    }
    select.value = tenantId || accounts.tenants[0]?.id || "";
    select.disabled = !accounts.tenants.length;
    if (!accounts.tenants.length) option(select, "", "No cached tenants", true);
    byId("subscription-filter").disabled = !accounts.subscriptions.length;
    fillSubscriptions(preferred);
    if (accounts.errors.length) {
        byId("error").textContent = accounts.errors.join("\n");
        byId("error").hidden = false;
    }
}

function clearChoices() {
    choicesReady = false;
    byId("resource").replaceChildren();
    option(byId("resource"), "", "Load resources for this subscription", true);
    byId("resource").disabled = true;
    fillModels();
    byId("loading").textContent = "Select Load resources to inspect this subscription.";
    render(current);
}

async function loadResources() {
    const draft = byId("provider").disabled ? null : {
        provider: byId("provider").value, resourceId: byId("resource").value, model: byId("model").value,
    };
    byId("loading").textContent = "Loading resources and models...";
    try {
        current = await request("discover", { subscription: byId("subscription").value || undefined });
    } catch (error) {
        byId("loading").textContent = "Could not load models. Try Load resources again.";
        throw error;
    }
    fillProviders(current, draft);
    render(current);
}

function fillProviders(state, draft) {
    const preferred = draft || state.preferred;
    const provider = byId("provider");
    provider.replaceChildren();
    option(provider, "azure", "Azure");
    option(provider, "openai", state.catalog.openai.available ? "OpenAI" : "OpenAI (API key required)", !state.catalog.openai.available);
    provider.value = preferred?.provider || state.selection?.provider || "azure";
    if (provider.selectedOptions[0]?.disabled) provider.value = "azure";
    provider.disabled = false;
    const resources = byId("resource");
    resources.replaceChildren();
    for (const resource of state.catalog.resources) {
        const available = resource.deployments.some(model => model.compatible);
        option(resources, resource.id, `${resource.name}${available ? "" : " (no GPT-Live model)"}`, !available);
    }
    const available = [...resources.options].filter(item => !item.disabled);
    resources.value = available.find(item => item.value === preferred?.resourceId)?.value || available[0]?.value || "";
    resources.disabled = !available.length;
    if (!resources.options.length) option(resources, "", "No resources found", true);
    else if (!available.length) {
        option(resources, "", "No compatible resources", true);
        resources.value = "";
    }
    choicesReady = true;
    fillModels(preferred?.model);
    byId("loading").textContent = "";
    byId("warnings").hidden = !state.catalog.errors.length;
    byId("warning-text").textContent = state.catalog.errors.join("\n\n");
}

function fillModels(preferred) {
    const model = byId("model");
    model.replaceChildren();
    const provider = byId("provider").value;
    const azure = provider === "azure";
    byId("discovery").hidden = !azure;
    byId("resource-field").hidden = !azure;
    byId("models-refresh").hidden = azure;
    byId("loading").hidden = !azure;
    setLogo("provider-logo", provider);
    byId("provider-help").textContent = azure ? "Uses your Azure CLI sign-in." : "Uses the API key from the app environment.";
    if (provider === "openai") {
        for (const name of current.catalog?.openai.models || []) option(model, name, name);
        byId("endpoint").textContent = "api.openai.com - API key from the app environment";
    } else {
        const resource = choicesReady ? current.catalog?.resources.find(item => item.id === byId("resource").value) : null;
        for (const deployment of resource?.deployments || []) {
            option(model, deployment.name, deployment.name + (deployment.compatible ? "" : ` (${deployment.state === "Succeeded" ? "not compatible" : deployment.state})`), !deployment.compatible);
        }
        byId("endpoint").textContent = resource?.endpoint || "No resource selected.";
    }
    const available = [...model.options].filter(item => !item.disabled);
    model.value = available.find(item => item.value === preferred)?.value || available[0]?.value || "";
    model.disabled = !available.length;
    if (!available.length) {
        option(model, "", azure && !choicesReady ? "Load resources first" : "No GPT-Live models available", true);
        model.value = "";
    }
    byId("model-help").textContent = available.length ? "Only GPT-Live models support voice."
        : azure ? "Load a resource with a deployed GPT-Live model." : "This API key has no available GPT-Live models.";
}

function renderPermissions(state) {
    const report = state.permissions;
    const checking = permissionBusy || state.permissionsChecking;
    byId("permission-refresh").disabled = busy || checking;
    byId("permission-status").textContent = checking ? "Checking..." : {
        granted: "Granted", denied: "Action needed", unknown: "Unknown", error: "Check failed",
    }[report?.status] || "Not checked";
    const signature = JSON.stringify(report);
    if (signature === permissionSignature) return;
    permissionSignature = signature;
    const host = report?.context?.hostApp;
    const process = report?.context?.current;
    byId("permission-host").textContent = host
        ? `Host app: ${host.name} (PID ${host.pid}). Extension PID: ${process.pid}.`
        : process ? `Extension PID: ${process.pid}. No host app was found in its process ancestry.`
            : "Current process identity is not available.";
    const list = byId("permission-list");
    list.replaceChildren();
    for (const item of report?.permissions || []) {
        const label = document.createElement("dt");
        label.textContent = item.name;
        const status = document.createElement("dd");
        status.textContent = item.granted === true ? "Granted" : item.granted === false ? "Not granted" : "Unknown";
        status.dataset.granted = String(item.granted);
        list.append(label, status);
    }
    const missing = report?.permissions?.filter(item => item.granted === false).map(item => item.name) || [];
    byId("permission-guidance").textContent = report?.error || (missing.length
        ? `Enable ${missing.join(" and ")} in System Settings > Privacy & Security for ${host?.name || "the app that starts thursday-agent"}. Event Synthesizing uses the Accessibility setting. Quit and reopen that app, then check again. Voice can still start.`
        : report?.status === "granted" ? ""
            : "Select Check permissions to read the current state.");
    byId("permission-time").textContent = report?.checkedAt
        ? `Last checked: ${new Date(report.checkedAt).toLocaleTimeString()}. Also checked before each start.` : "";
    byId("permission-process").textContent = report?.context
        ? report.context.ancestors.map(item => `PID ${item.pid} (parent ${item.parentPid})\n${item.executable}`).join("\n\n")
            + (report.probe?.launcherPid ? `\n\nPermission check launcher PID: ${report.probe.launcherPid}` : "")
        : report?.error || "No process check yet.";
}

function render(state) {
    current = state;
    const selection = state.selection;
    byId("selected-model").textContent = selection?.model || "Select a voice model";
    byId("selected-provider").textContent = selection
        ? `${selection.provider === "azure" ? "Azure" : "OpenAI"} voice model`
        : "Choose a provider and model in Setup.";
    setLogo("selected-logo", selection?.provider);
    byId("edit-setup").textContent = selection ? "Change" : "Set up";
    byId("voice-state").textContent = { stopped: "Stopped", starting: "Starting", running: "Running", stopping: "Stopping", failed: "Failed" }[state.process.status] || state.process.status;
    byId("voice-state").dataset.state = state.process.status;
    byId("process-log").textContent = state.process.log || "Not started.";
    const running = ["running", "starting", "stopping"].includes(state.process.status);
    byId("start").hidden = running;
    byId("stop").hidden = !running;
    byId("start").disabled = busy || running || !selection;
    byId("stop").disabled = busy || !running;
    byId("start").textContent = pendingAction === "start" ? "Starting..." : "Start thursday-agent";
    byId("stop").textContent = pendingAction === "stop" || state.process.status === "stopping" ? "Stopping..." : "Stop thursday-agent";
    byId("check").disabled = busy || running || !selection;
    byId("check").textContent = pendingAction === "check" ? "Checking..." : "Check connection";
    byId("check-result").textContent = running ? "Stop thursday-agent before you check the connection."
        : !selection ? "Save a model in Setup first." : connectionResult;
    byId("save").disabled = busy || running || byId("model").disabled;
    byId("setup-fields").disabled = busy;
    byId("saved").textContent = running ? "Stop thursday-agent before you save model changes." : "";
    const needsPermissions = state.permissions && state.permissions.status !== "granted";
    byId("voice-notice").hidden = !needsPermissions && state.process.status !== "failed";
    byId("open-diagnosis").hidden = byId("voice-notice").hidden;
    byId("voice-notice").textContent = state.process.status === "failed"
        ? "thursday-agent stopped with an error. Open Diagnosis for the process log."
        : state.permissions?.status === "denied"
            ? "Mac controls need permissions. Voice can still start. See Diagnosis."
            : "Mac permissions are not confirmed. Voice can still start. See Diagnosis.";
    byId("refresh").disabled = busy || byId("subscription").disabled;
    byId("accounts-refresh").disabled = busy;
    renderPermissions(state);
}

async function action(work, name = "") {
    if (busy) return;
    busy = true;
    pendingAction = name;
    byId("error").hidden = true;
    if (current) render(current);
    try { await work(); return true; }
    catch (error) { byId("error").textContent = error.message; byId("error").hidden = false; return false; }
    finally { busy = false; pendingAction = ""; if (current) render(current); }
}

byId("discovery").addEventListener("submit", event => {
    event.preventDefault();
    action(loadResources);
});
byId("accounts-refresh").addEventListener("click", () => action(async () => { await loadAccounts(); clearChoices(); }));
byId("models-refresh").addEventListener("click", () => action(loadResources));
byId("tenant").addEventListener("change", () => { byId("subscription-filter").value = ""; fillSubscriptions(); clearChoices(); });
byId("subscription-filter").addEventListener("input", () => { fillSubscriptions(); clearChoices(); });
byId("subscription").addEventListener("change", clearChoices);
byId("provider").addEventListener("change", () => { fillModels(); render(current); });
byId("resource").addEventListener("change", () => { fillModels(); render(current); });
byId("model-form").addEventListener("submit", event => {
    event.preventDefault();
    action(async () => {
        const provider = byId("provider").value;
        render(await request("select", {
            provider,
            resourceId: provider === "azure" ? byId("resource").value : undefined,
            model: byId("model").value,
        }));
        connectionResult = "";
        showTab("voice");
    });
});
for (const name of ["start", "stop"]) byId(name).addEventListener("click", () => {
    action(async () => render(await request(name, {})), name).then(success => {
        if (success && !byId("panel-voice").hidden) byId(name === "start" ? "stop" : "start").focus();
    });
});
byId("check").addEventListener("click", () => action(async () => {
    connectionResult = "Opening a GPT-Live session...";
    render(current);
    try { connectionResult = (await request("check", {})).message; }
    catch (error) { connectionResult = "Connection check failed."; throw error; }
}, "check"));
byId("permission-refresh").addEventListener("click", () => action(async () => {
    permissionBusy = true;
    render(current);
    try {
        const permissions = await request("permissions", {});
        current = { ...current, permissions, permissionsChecking: false };
    } catch (error) {
        current = {
            ...current, permissionsChecking: false,
            permissions: { ...current.permissions, status: "error", permissions: [], error: error.message },
        };
        throw error;
    } finally {
        permissionBusy = false;
    }
}));
async function poll() {
    if (!busy) {
        try {
            const next = await request("state");
            if (!busy) render(next);
        } catch (error) {
            byId("error").textContent = `Canvas connection failed: ${error.message}. Reopen the canvas.`;
            byId("error").hidden = false;
        }
    }
    setTimeout(poll, 1500);
}
action(async () => {
    current = await request("state");
    render(current);
    showTab(current.selection ? "voice" : "setup", false);
    try { await loadAccounts(); }
    catch (error) {
        for (const id of ["tenant", "subscription"]) {
            byId(id).replaceChildren();
            option(byId(id), "", "Azure accounts unavailable", true);
            byId(id).disabled = true;
        }
        byId("error").textContent = `Azure accounts could not be loaded: ${error.message}`;
        byId("error").hidden = false;
    }
    await loadResources();
}).then(poll);
