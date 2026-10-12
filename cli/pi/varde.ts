/**
 * varde for pi. pi has no shell hooks, so this forwards its session
 * events to `varde hook --agent pi <event>`, as the Claude plugin's
 * hooks.json does for Claude. Nothing else lives here: what an event
 * means is decided in varde's pi adapter.
 *
 * Install: mkdir -p ~/.pi/agent/extensions && ln -s "$PWD/varde.ts" ~/.pi/agent/extensions/varde.ts
 */

import { spawnSync } from "node:child_process";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
	// What varde hands the session (as it starts, after a compaction, or
	// with the prompt that makes its task), sent with its next prompt.
	let context: string | undefined;
	const keep = (reply: { context?: string }) => {
		if (reply.context) context = context ? `${context}\n\n${reply.context}` : reply.context;
	};

	// Synchronous, so events reach varde in order; `varde hook` takes a
	// few milliseconds, always exits 0 and prints a JSON object.
	const hook = (event: string, ctx: ExtensionContext, extra: Record<string, unknown> = {}) => {
		const payload = {
			session_id: ctx.sessionManager.getSessionId(),
			session_file: ctx.sessionManager.getSessionFile(),
			cwd: ctx.cwd,
			...extra,
		};
		const out = spawnSync("varde", ["hook", "--agent", "pi", event], {
			input: JSON.stringify(payload),
			encoding: "utf8",
			timeout: 5000,
		});
		try {
			return JSON.parse(out.stdout || "{}");
		} catch {
			return {};
		}
	};

	pi.on("session_start", (event, ctx) => {
		// A new session: what was kept for the one before is not its.
		context = undefined;
		// pi says `startup` for any session it opens as it starts, a
		// resumed or forked one too; what the session holds tells them apart.
		const header = ctx.sessionManager.getHeader();
		const said = ctx.sessionManager
			.getEntries()
			.filter((e) => e.type === "message" && (e.message.role === "user" || e.message.role === "assistant"));
		keep(
			hook("session_start", ctx, {
				reason: event.reason,
				created: header?.timestamp,
				parent_session: header?.parentSession,
				last_message: said.at(-1)?.timestamp,
			}),
		);
	});
	pi.on("before_agent_start", (event, ctx) => {
		keep(hook("before_agent_start", ctx, { prompt: event.prompt }));
		if (!context) return;
		const content = context;
		context = undefined;
		return { message: { customType: "varde", content, display: false } };
	});
	pi.on("agent_settled", (event, ctx) => void hook("agent_settled", ctx, { aborted: event.aborted }));
	pi.on("ui_prompt_start", (_event, ctx) => void hook("ui_prompt_start", ctx));
	pi.on("ui_prompt_end", (_event, ctx) => void hook("ui_prompt_end", ctx));
	pi.on("session_before_compact", (_event, ctx) => void hook("session_before_compact", ctx));
	pi.on("session_compact", (_event, ctx) => keep(hook("session_compact", ctx)));
	pi.on("session_shutdown", (event, ctx) => void hook("session_shutdown", ctx, { reason: event.reason }));
}
