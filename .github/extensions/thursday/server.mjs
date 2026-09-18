import { createServer } from "node:http";
import { randomBytes, timingSafeEqual } from "node:crypto";
import { readFile } from "node:fs/promises";

const assetTypes = new Map([
    ["", "text/html"],
    ["ui.js", "text/javascript"],
    ["style.css", "text/css"],
    ["logo.svg", "image/svg+xml"],
]);

function equal(a, b) {
    const left = Buffer.from(a || "");
    const right = Buffer.from(b);
    return left.length === right.length && timingSafeEqual(left, right);
}

async function body(req) {
    let bytes = 0;
    const chunks = [];
    for await (const chunk of req) {
        bytes += chunk.length;
        if (bytes > 20_000) throw new Error("Request is too large.");
        chunks.push(chunk);
    }
    const text = Buffer.concat(chunks).toString("utf8");
    return text ? JSON.parse(text) : {};
}

export async function startServer(api) {
    const token = randomBytes(32).toString("hex");
    const prefix = `/canvas/${token}/`;
    let origin;
    const server = createServer(async (req, res) => {
        res.setHeader("Cache-Control", "no-store");
        res.setHeader("Referrer-Policy", "no-referrer");
        res.setHeader("X-Content-Type-Options", "nosniff");
        const json = (status, value) => {
            res.writeHead(status, { "Content-Type": "application/json" });
            res.end(JSON.stringify(value));
        };
        try {
            if (req.headers.host !== new URL(origin).host) return json(403, { error: "Invalid host." });
            if (req.headers.origin && req.headers.origin !== origin && req.headers.origin !== "null") {
                return json(403, { error: "Cross-origin requests are not allowed." });
            }
            const path = new URL(req.url, origin).pathname;
            const canvas = path.startsWith(prefix);
            if (!canvas && !equal(req.headers.authorization, `Bearer ${token}`)) {
                return json(401, { error: "Authentication required." });
            }
            const route = canvas ? path.slice(prefix.length) : path.slice(1);
            if (canvas && req.method === "GET" && assetTypes.has(route)) {
                const file = route || "index.html";
                res.setHeader("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; form-action 'self'");
                res.writeHead(200, { "Content-Type": `${assetTypes.get(route)}; charset=utf-8` });
                return res.end(await readFile(new URL(file, import.meta.url)));
            }
            if (req.method === "GET" && route === "state") return json(200, await api.state());
            if (canvas && req.method === "GET" && route === "accounts") return json(200, await api.accounts());
            if (req.method === "GET" && route.startsWith("requests/")) return json(200, api.request(route.slice(9)));
            if (req.method !== "POST") return json(404, { error: "Not found." });
            if (!req.headers["content-type"]?.startsWith("application/json")) {
                return json(415, { error: "Use application/json." });
            }
            const input = await body(req);
            if (route === "prompt") return json(202, await api.prompt(input.prompt));
            if (!canvas) return json(403, { error: "This operation requires the canvas." });
            if (route === "discover") return json(200, await api.discover(input.subscription));
            if (route === "select") return json(200, await api.select(input));
            if (route === "start") return json(200, await api.start());
            if (route === "stop") return json(200, await api.stop());
            if (route === "check") return json(200, await api.check());
            if (route === "permissions") return json(200, await api.permissions());
            return json(404, { error: "Not found." });
        } catch (error) {
            if (!res.headersSent) json(400, { error: error.message });
            else res.destroy();
        }
    });
    await new Promise((resolve, reject) => {
        server.once("error", reject);
        server.listen(0, "127.0.0.1", resolve);
    });
    origin = `http://127.0.0.1:${server.address().port}`;
    return { server, token, url: `${origin}/`, canvasUrl: `${origin}${prefix}` };
}
