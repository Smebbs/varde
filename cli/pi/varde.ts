/**
 * varde for pi. pi has no shell hooks, so this forwards its session
 * events to `varde hook --agent pi <event>`, as the Claude plugin's
 * hooks.json does for Claude, and takes in what `varde say` sends the
 * session. Nothing else lives here: what an event means is decided in
 * varde's pi adapter.
 *
 * Install: mkdir -p ~/.pi/agent/extensions && ln -s "$PWD/varde.ts" ~/.pi/agent/extensions/varde.ts
 */

import { spawnSync } from "node:child_process";
import { mkdirSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:net";
import { join } from "node:path";
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

	// `varde say` reaches the session here while it is open: varde's pi
	// adapter writes a line to this process's inbox, and the message goes
	// in as one from the user, a turn when idle, steered into the turn
	// under way when urgent, or after it. Only for the session it was
	// meant for: one that moved on (a new or resumed session) drops it.
	let session: string | undefined;
	let inbox: Server | undefined;
	const listen = () => {
		const dir = process.env.XDG_RUNTIME_DIR;
		if (inbox || !dir) return;
		const path = join(dir, "varde", `pi-${process.pid}.sock`);
		try {
			mkdirSync(join(dir, "varde"), { recursive: true, mode: 0o700 });
			rmSync(path, { force: true });
		} catch {
			return;
		}
		inbox = createServer((conn) => {
			let buf = "";
			conn.on("data", (d) => {
				buf += d.toString();
				let nl: number;
				while ((nl = buf.indexOf("\n")) >= 0) {
					const line = buf.slice(0, nl);
					buf = buf.slice(nl + 1);
					// varde counts it in only on "ok": else the session's
					// next hook carries it.
					let ok = false;
					try {
						const m = JSON.parse(line) as { session_id?: string; text?: string; urgent?: boolean };
						if (m.text && m.session_id === session) {
							pi.sendUserMessage(m.text, { deliverAs: m.urgent ? "steer" : "followUp" });
							ok = true;
						}
					} catch {
						// A line that is not a message is no message.
					}
					conn.write(ok ? "ok\n" : "dropped\n");
				}
			});
		});
		inbox.on("error", () => (inbox = undefined));
		inbox.listen(path);
		process.once("exit", () => rmSync(path, { force: true }));
	};

	pi.on("session_start", (event, ctx) => {
		session = ctx.sessionManager.getSessionId();
		listen();
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
	pi.on("session_shutdown", (event, ctx) => {
		hook("session_shutdown", ctx, { reason: event.reason });
		session = undefined;
	});
}
