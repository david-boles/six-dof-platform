/**
 * Shared helpers for the approval extensions.
 *
 * Lives in a subdirectory without index.ts, so Pi does not load it as an extension
 * (discovery is one level deep: .pi/extensions/*.ts and .pi/extensions/* /index.ts).
 */

import type { ExtensionContext } from "@earendil-works/pi-coding-agent";

/**
 * Characters that can hide, reorder, or disguise text in a terminal:
 *  - \p{Cc}: C0/C1 control characters incl. ESC (ANSI sequences), CR, BS, DEL
 *  - \p{Cf}: format characters: bidi overrides/isolates, zero-width chars, soft hyphen, BOM, tag chars
 *  - \p{Zl}, \p{Zp}: Unicode line/paragraph separators
 *  - \p{Zs} other than U+0020: look-alike spaces (NBSP etc.)
 *  - variation selectors, combining grapheme joiner, Hangul fillers, Mongolian vowel separator
 * "\n" is handled separately; "\t" is kept (the TUI expands it to spaces).
 */
const SUSPICIOUS =
	/(?![\n\t ])[\p{Cc}\p{Cf}\p{Zl}\p{Zp}\p{Zs}\u034F\u115F\u1160\u17B4\u17B5\u180B-\u180F\u3164\uFE00-\uFE0F\uFFA0\u{E0100}-\u{E01EF}]/gu;

/** Make arbitrary text safe to show in an approval prompt: every suspicious char becomes a visible escape. */
export function sanitize(text: string, { singleLine = false } = {}): string {
	let out = text.replace(SUSPICIOUS, (ch) => `⟨U+${ch.codePointAt(0)!.toString(16).toUpperCase().padStart(4, "0")}⟩`);
	if (singleLine) out = out.replace(/\n/g, "⟨\\n⟩");
	return out;
}

const PREVIEW_MAX_LINES = 20;
const PREVIEW_MAX_LINE_CHARS = 300;
const PREVIEW_MAX_CHARS = 2000;

/** Bounded preview, so the prompt can never be pushed off-screen by padding or a huge payload. */
export function preview(text: string): { text: string; truncated: boolean; lines: number; chars: number } {
	const safe = sanitize(text);
	const lines = safe.split("\n");
	const shown: string[] = [];
	let truncated = false;
	let total = 0;
	for (const line of lines) {
		if (shown.length >= PREVIEW_MAX_LINES || total >= PREVIEW_MAX_CHARS) {
			truncated = true;
			break;
		}
		let l = line;
		if (l.length > PREVIEW_MAX_LINE_CHARS) {
			l = `${l.slice(0, PREVIEW_MAX_LINE_CHARS)} …[line truncated]`;
			truncated = true;
		}
		shown.push(l);
		total += l.length;
	}
	return { text: shown.join("\n"), truncated, lines: lines.length, chars: text.length };
}

const VIEW_FULL = "View full text";

/**
 * Ask the user to approve. `body` is untrusted (agent-controlled) and is sanitized and bounded.
 * Returns true only on an explicit "Yes".
 */
export async function approve(ctx: ExtensionContext, heading: string, body: string): Promise<boolean> {
	const p = preview(body);
	const indented = p.text
		.split("\n")
		.map((l) => `  ${l}`)
		.join("\n");
	let title = `${heading}\n\n${indented}\n`;
	const options = ["Yes", "No"];
	if (p.truncated) {
		title += `\n  ⚠ PREVIEW TRUNCATED: full text is ${p.lines} lines / ${p.chars} chars. Choose "${VIEW_FULL}" to inspect it.\n`;
		options.push(VIEW_FULL);
	}

	while (true) {
		const choice = await ctx.ui.select(title, options);
		if (choice === VIEW_FULL) {
			// Edits made in the editor are ignored; it is only a viewer.
			await ctx.ui.editor(`${heading} (read-only view, edits are ignored)`, sanitize(body));
			continue;
		}
		return choice === "Yes";
	}
}
