/**
 * Path Approval Extension
 *
 * Requires user approval before the agent's file tools (read, write, edit, grep, find, ls)
 * touch:
 *   1. anything outside the project folder, or
 *   2. Pi configuration inside the project folder:
 *      - .pi/** (settings, extensions, skills, prompts, themes, SYSTEM.md, ...)
 *      - .agents/** (project skills)
 *      - AGENTS.md, AGENTS.MD, AGENTS.override.md, CLAUDE.md, CLAUDE.MD (any directory)
 *   3. unusual URL-like paths (e.g. "http://...") that Pi might interpret in the future.
 *
 * Paths are resolved exactly like Pi's built-in tools (Unicode spaces normalized, leading "@"
 * stripped, "~" expanded, "file://" URLs converted, relative to cwd). As defense in depth, the
 * trimmed input and the read tool's filename fallbacks (macOS AM/PM, NFD, curly quote) are checked
 * too; the most restrictive verdict wins. Every candidate is then resolved through symlinks,
 * including dangling symlinks (whose target doesn't exist yet), so a link inside the project
 * pointing outside still prompts.
 *
 * Fails closed: without a UI (print/JSON mode), or if a path can't be checked, the call is blocked.
 *
 * Not covered: bash commands (see command-approval.ts), other tools (see tool-approval.ts),
 * grep searching a directory that *contains* config files (only the search root is checked), and
 * a symlink swapped in between this check and the tool running (needs an approved command).
 */

import { lstatSync, readlinkSync, realpathSync } from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { approve, sanitize } from "./lib/approval.ts";

const READ_TOOLS = new Set(["read", "grep", "find", "ls"]);
const WRITE_TOOLS = new Set(["write", "edit"]);
const CONFIG_DIRS = new Set([".pi", ".agents"]);
const CONTEXT_FILES = new Set(["AGENTS.md", "AGENTS.MD", "AGENTS.override.md", "CLAUDE.md", "CLAUDE.MD"]);

// Same set Pi's normalizePath() replaces with " ".
const UNICODE_SPACES = /[\u00A0\u2000-\u200A\u202F\u205F\u3000]/g;
const URL_LIKE = /^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//;
const MAX_SYMLINK_HOPS = 40;

/** Mirror of Pi's resolveToCwd()/resolvePath() (utils/paths.js, Pi 1.5.2) on Linux. */
export function resolveLikePi(input: string, cwd: string): string {
	let p = input.replace(UNICODE_SPACES, " ");
	if (p.startsWith("@")) p = p.slice(1);
	if (p === "~") p = homedir();
	else if (p.startsWith("~/")) p = join(homedir(), p.slice(2));
	else if (/^file:\/\//.test(p)) p = fileURLToPath(p); // throws on non-local hosts -> blocked
	return isAbsolute(p) ? resolve(p) : resolve(cwd, p);
}

/** Read tool fallbacks (path-utils.js resolveReadPath) tried when the exact path doesn't exist. */
function readVariants(p: string): string[] {
	const nfd = p.normalize("NFD");
	return [p.replace(/ (AM|PM)\./gi, "\u202F$1."), nfd, p.replace(/'/g, "\u2019"), nfd.replace(/'/g, "\u2019")];
}

function isEnoent(err: unknown): boolean {
	const code = (err as NodeJS.ErrnoException)?.code;
	return code === "ENOENT" || code === "ENOTDIR";
}

/**
 * Resolve symlinks like the kernel would when opening/creating `p`. Unlike realpathSync, this
 * also follows dangling symlinks (write would create their target). Missing components are kept
 * as-is. Any other error (EACCES, ELOOP, too many hops, ...) throws, which blocks the call.
 */
function realPath(p: string, hops = 0): string {
	if (hops > MAX_SYMLINK_HOPS) throw new Error(`too many symlinks resolving ${p}`);
	try {
		return realpathSync(p);
	} catch (err) {
		if (!isEnoent(err)) throw err;
	}
	const parent = dirname(p);
	if (parent === p) return p;
	const realParent = realPath(parent, hops);
	const here = join(realParent, basename(p));
	let isLink = false;
	try {
		isLink = lstatSync(here).isSymbolicLink();
	} catch (err) {
		if (!isEnoent(err)) throw err;
	}
	if (!isLink) return here;
	return realPath(resolve(realParent, readlinkSync(here)), hops + 1);
}

type Reason = "outside project" | "Pi configuration" | "unusual path";
type Verdict = { reason: Reason } | undefined;
const SEVERITY: Record<Reason, number> = { "Pi configuration": 1, "unusual path": 2, "outside project": 3 };

function classify(target: string, projectRoot: string): Verdict {
	const rel = relative(projectRoot, target);
	if (rel === ".." || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
		return { reason: "outside project" };
	}
	if (rel === "") return undefined;
	const parts = rel.split(sep);
	if (CONFIG_DIRS.has(parts[0]) || CONTEXT_FILES.has(parts[parts.length - 1])) {
		return { reason: "Pi configuration" };
	}
	return undefined;
}

function worst(a: Verdict, b: Verdict): Verdict {
	if (!a) return b;
	if (!b) return a;
	return SEVERITY[b.reason] > SEVERITY[a.reason] ? b : a;
}

/** Glob patterns for find/grep can also escape the search root. */
function patternEscapes(pattern: string | undefined): boolean {
	if (!pattern) return false;
	const p = pattern.trim().replace(/^@/, "");
	return isAbsolute(p) || p.startsWith("~") || p.split(/[\\/]/).includes("..");
}

/** Check one tool path. Returns the verdict and the resolved target Pi will most likely use. */
export function checkPath(input: string, cwd: string, tool: string): { verdict: Verdict; target: string } {
	const projectRoot = realPath(resolve(cwd));

	const primary = resolveLikePi(input, cwd);
	const candidates = new Set<string>([primary, resolveLikePi(input.trim(), cwd)]);
	if (tool === "read") for (const v of readVariants(primary)) candidates.add(v);

	let verdict: Verdict;
	for (const c of candidates) verdict = worst(verdict, classify(realPath(c), projectRoot));

	const stripped = input.replace(UNICODE_SPACES, " ").trim().replace(/^@/, "");
	if (URL_LIKE.test(stripped) && !/^file:\/\//.test(stripped)) {
		verdict = worst(verdict, { reason: "unusual path" });
	}
	return { verdict, target: realPath(primary) };
}

export default function (pi: ExtensionAPI) {
	pi.on("tool_call", async (event, ctx) => {
		const tool = event.toolName;
		if (!READ_TOOLS.has(tool) && !WRITE_TOOLS.has(tool)) return undefined;

		const input = event.input as { path?: unknown; pattern?: unknown; glob?: unknown };
		const action = WRITE_TOOLS.has(tool) ? "write" : "read";

		let verdict: Verdict;
		let target: string;
		try {
			const rawPath = input.path ?? ".";
			if (typeof rawPath !== "string") throw new Error("path is not a string");
			({ verdict, target } = checkPath(rawPath, ctx.cwd, tool));
			if (rawPath !== target) target = `${target}  (requested: ${rawPath})`;

			const pattern = tool === "find" ? input.pattern : tool === "grep" ? input.glob : undefined;
			if (pattern !== undefined && typeof pattern !== "string") throw new Error("pattern is not a string");
			if (patternEscapes(pattern)) {
				verdict = worst(verdict, { reason: "outside project" });
				target = `${target}  (pattern: ${pattern})`;
			}
		} catch (err) {
			return { block: true, reason: `Path approval check failed: ${String(err)}` };
		}

		if (!verdict) return undefined;

		if (!ctx.hasUI) {
			return {
				block: true,
				reason: `${tool} on ${sanitize(target, { singleLine: true })} (${verdict.reason}) requires user approval, but no UI is available`,
			};
		}

		if (!(await approve(ctx, `Allow ${action} (${tool}) — ${verdict.reason}?`, sanitize(target, { singleLine: true })))) {
			return { block: true, reason: `${tool} on ${sanitize(target, { singleLine: true })} was not approved by the user` };
		}
		return undefined;
	});
}
