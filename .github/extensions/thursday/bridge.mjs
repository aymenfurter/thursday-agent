import { randomUUID } from "node:crypto";

const REQUEST_TIMEOUT_MS = 180_000;
const pending = job => ["queued", "running"].includes(job.status);

const REPLY_INSTRUCTIONS = `This is an agent-to-agent request from thursday-agent, not a user-facing chat task.
Reply as quickly as possible through the thursday_reply tool, using the requestId below and a concise, complete plain-text response.
Use the Copilot App tools only as needed to get the answer. As soon as the answer is available, call thursday_reply immediately; do not wait for the end of this turn.
Do not narrate, post the answer in chat, render widgets, open canvases, or create presentation files. thursday-agent will return the response to its caller.
If you cannot complete the request, use thursday_reply to return the blocker promptly.
After thursday_reply succeeds, stop without a user-facing message.
Do not call thursday_send_prompt recursively. Do not change or message other sessions unless the request asks you to.`;

export class PromptBridge {
    constructor(session, previous = [], changed = () => {}, now = Date.now) {
        this.session = session;
        this.changed = changed;
        this.now = now;
        this.cleanup = Promise.resolve();
        this.jobs = new Map(previous.map(job => [job.id, ["queued", "running"].includes(job.status)
            ? { ...job, status: "failed", error: "The extension restarted. Check the app conversation before you resend this request." }
            : job]));
        this.unsubscribe = session.on(event => this.event(event));
    }

    async removeQueuedRequests(messageIds) {
        if (!messageIds.length) return;
        const { items } = await this.session.rpc.queue.pendingItems();
        for (const item of items) {
            if (messageIds.includes(item.messageId)) await this.session.rpc.queue.removeAt({ id: item.id });
        }
    }

    recover() {
        return this.removeQueuedRequests([...this.jobs.values()]
            .filter(job => !pending(job)).map(job => job.messageId).filter(Boolean));
    }

    expire() {
        const expired = [];
        for (const job of this.jobs.values()) {
            if (pending(job) && this.now() >= Date.parse(job.expiresAt)) {
                job.status = "failed";
                job.error = "The app did not reply within three minutes. This request expired; this does not mean the app is still working.";
                job.completedAt = new Date(this.now()).toISOString();
                expired.push(job);
            }
        }
        if (expired.length) {
            this.changed([...this.jobs.values()]);
            this.cleanup = this.cleanup.then(() => this.removeQueuedRequests(expired.map(job => job.messageId).filter(Boolean)))
                .catch(error => this.session.log(`thursday-agent expired-request cleanup failed: ${error.message}`, { level: "error" }));
        }
    }

    list() {
        this.expire();
        return [...this.jobs.values()];
    }

    get(id) {
        this.expire();
        const job = this.jobs.get(id);
        if (!job) throw new Error("Request not found. The extension may have restarted.");
        return job;
    }

    reply(requestId, response) {
        if (typeof response !== "string" || !response.trim() || Buffer.byteLength(response) > 16_000) {
            throw new Error("Return a plain-text response with 1 to 16000 bytes.");
        }
        const job = this.get(requestId);
        const text = response.trim();
        if (job.status === "completed" && job.response === text) {
            return { requestId, status: "completed" };
        }
        if (!["queued", "running"].includes(job.status)) {
            throw new Error(`Request is ${job.status}; its result cannot be replaced.`);
        }
        job.response = text;
        job.status = "completed";
        job.completedAt = new Date(this.now()).toISOString();
        this.changed(this.list());
        return { requestId, status: "completed" };
    }

    async send(prompt) {
        if (typeof prompt !== "string" || !prompt.trim() || Buffer.byteLength(prompt) > 16_000) {
            throw new Error("Enter a prompt with 1 to 16000 bytes.");
        }
        this.expire();
        const job = {
            id: randomUUID(), prompt: prompt.trim(), status: "queued",
            createdAt: new Date(this.now()).toISOString(),
            expiresAt: new Date(this.now() + REQUEST_TIMEOUT_MS).toISOString(),
            sessionId: this.session.sessionId,
        };
        this.jobs.set(job.id, job);
        for (const [id, previous] of this.jobs) {
            if (this.jobs.size <= 50) break;
            if (!pending(previous)) this.jobs.delete(id);
        }
        this.changed(this.list());
        try {
            job.messageId = await this.session.send({
                prompt: `${REPLY_INSTRUCTIONS}\n\nrequestId: ${job.id}\nexpiresAt: ${job.expiresAt}\nIf this request has expired, do not perform its work or send a reply.\n\nRequest:\n${job.prompt}`,
                displayPrompt: "thursday-agent request",
                source: `thursday-${job.id}`,
                mode: "immediate",
            });
        } catch (error) {
            if (pending(job)) {
                job.status = "failed";
                job.error = error.message;
            }
        }
        this.changed(this.list());
        return job;
    }

    event(event) {
        const data = event.data || {};
        const job = this.list().find(item => ["queued", "running"].includes(item.status)
            && ((data.source === `thursday-${item.id}`)
                || (item.messageId && [data.messageId, data.originatingMessageId].includes(item.messageId))));
        if (event.type === "user.message" && job) {
            job.messageId = data.messageId || job.messageId;
            job.status = "running";
        } else if (event.type === "session.error") {
            for (const pending of this.list().filter(item => item.status === "running")) {
                pending.status = "failed";
                pending.error = data.message || "Copilot App reported a session error.";
            }
        } else {
            return;
        }
        this.changed(this.list());
    }
}
