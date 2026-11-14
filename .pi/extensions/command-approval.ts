/**
 * Command Approval Extension
 *
 * Requires explicit user approval before the agent runs ANY shell command (bash or powershell).
 * The command is shown sanitized (control/ANSI/bidi/invisible chars made visible) and as a bounded
 * preview, with a full-text viewer for long commands, so it can't be disguised in the prompt.
 * Fails closed: if no UI is available (print/JSON mode), commands are blocked.
 * User-initiated `!` / `!!` commands are not gated.
 */

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { approve } from "./lib/approval.ts";

const SHELL_TOOLS = new Set(["bash", "powershell"]);

export default function (pi: ExtensionAPI) {
	pi.on("tool_call", async (event, ctx) => {
		if (!SHELL_TOOLS.has(event.toolName)) return undefined;

		const command = (event.input as { command?: unknown }).command;
		if (typeof command !== "string") {
			return { block: true, reason: "Command blocked: missing or invalid command" };
		}

		if (!ctx.hasUI) {
			return { block: true, reason: "Command blocked: user approval required but no UI is available" };
		}

		if (!(await approve(ctx, `Run ${event.toolName} command?`, command))) {
			return { block: true, reason: "Command was not approved by the user" };
		}
		return undefined;
	});
}
