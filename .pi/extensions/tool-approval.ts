/**
 * Tool Approval Extension (catch-all)
 *
 * Requires user approval for every tool call that no other approval extension handles:
 * MCP tools, tools registered by other extensions, and any built-in tool added in the future.
 *
 * Handled elsewhere (passed through here):
 *   - read, grep, find, ls, write, edit  -> path-approval.ts
 *   - bash, powershell                   -> command-approval.ts
 *   - codemode, tool_search              -> orchestrators only; every tool they call goes through
 *                                           tool_call again (as a nested call) and is gated there.
 *
 * Fails closed: without a UI (print/JSON mode), unknown tools are blocked.
 */

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { approve } from "./lib/approval.ts";

export const HANDLED_ELSEWHERE = new Set([
	"read",
	"grep",
	"find",
	"ls",
	"write",
	"edit",
	"bash",
	"powershell",
	"codemode",
	"tool_search",
]);

function describeInput(input: unknown): string {
	try {
		return JSON.stringify(input, null, 2) ?? String(input);
	} catch {
		return String(input);
	}
}

export default function (pi: ExtensionAPI) {
	pi.on("tool_call", async (event, ctx) => {
		if (HANDLED_ELSEWHERE.has(event.toolName)) return undefined;

		if (!ctx.hasUI) {
			return { block: true, reason: `Tool ${event.toolName} requires user approval, but no UI is available` };
		}

		// Tool name is agent-influenced (e.g. MCP server/tool names), so it goes through the sanitizer too.
		const body = `tool: ${event.toolName}\n\n${describeInput(event.input)}`;
		if (!(await approve(ctx, "Allow tool call?", body))) {
			return { block: true, reason: `Tool ${event.toolName} was not approved by the user` };
		}
		return undefined;
	});
}
