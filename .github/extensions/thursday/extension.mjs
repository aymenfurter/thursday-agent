import { joinSession } from "@github/copilot-sdk/extension";

// A voice worker already has ask_copilot_app. Do not create a second app bridge
// inside the worker's own CLI session.
if (process.env.THURSDAY_APP_BRIDGE) {
    await joinSession({});
} else {
    await import("./main.mjs");
}
