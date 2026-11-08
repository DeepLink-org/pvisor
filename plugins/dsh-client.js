/* DSH's browser module protocol. No build/install hook or bundled React copy.
 * This file is the shipped source artifact; module effects stay inside factory.
 */
window.__ModuleLoader__.load({
  id: "pvisor-agent-plugins",
  factory(require) {
    const React = require("react");
    const { Button, Pill, Tag } = require("@deepseek-ai/dsh-client-ui-primitives");
    const { createElement: h, useEffect, useMemo, useState } = React;
    const ROUTE = "api/pvisor/monitor";
    const TAB_ID = "pvisor-agent-plugins/monitor";

    // Display heuristics only. Never evaluate, re-launch or authorize commands
    // using a title or this deliberately lossy shell-word preview.
    function displayArgs(task) {
      return Array.isArray(task?.command) ? task.command.filter((arg) => typeof arg === "string") : [];
    }
    function taskTitle(task) {
      if (typeof task?.name === "string" && task.name.trim()) return task.name.trim().slice(0, 100);
      let args = displayArgs(task);
      const basename = (value) => value?.split(/[\\/]/).at(-1) || "";
      if (/^(?:sh|bash|dash|zsh|fish)$/.test(basename(args[0])) && /^-[a-z]*c[a-z]*$/.test(args[1] || "")) {
        const script = args[2] || "";
        if (!/[;&|<>\n]/.test(script)) {
          args = (script.match(/"[^"]*"|'[^']*'|[^\s]+/g) || []).map((part) =>
            /^(['"]).*\1$/.test(part) ? part.slice(1, -1) : part);
        }
      }
      const script = args.find((arg) => /\.(?:sh|py|mjs|js|rb|pl)$/.test(arg));
      if (script) {
        const name = basename(script).replace(/\.[^.]+$/, "");
        const variant = args.at(-1);
        if (/^(work|run|task|main)$/.test(name) && variant !== script &&
            /^[\p{L}\p{N}_.:-]{1,80}$/u.test(variant || "")) return variant;
        return name;
      }
      const program = basename(args[0]);
      if (["cargo", "git", "npm", "pytest", "make"].includes(program)) {
        return [program, args[1]?.startsWith("-") ? null : args[1]].filter(Boolean).join(" ").slice(0, 100);
      }
      return program || `任务 ${task?.task?.slice(0, 8) || ""}`.trim();
    }
    function commandText(task) {
      return displayArgs(task).join(" ").replace(/\s+/g, " ").trim() + (task?.command_truncated ? " …" : "");
    }
    function mainStatus(task) {
      if (task?.live || task?.state === "running") return ["运行中", "info"];
      if (task?.state === "failed") return ["失败", "danger"];
      if (task?.state === "cancelled") return ["已取消", "warning"];
      if (["discarded", "dropped"].includes(task?.filesystem)) return ["已清理", "quiet"];
      if (task?.filesystem === "applied") return ["已接收", "success"];
      if (task?.state === "completed") return task.filesystem === "staged" && task.changed_files > 0 ?
        ["待审阅", "warning"] : ["已完成", "quiet"];
      return ["状态未知", "warning"];
    }
    function duration(task, now = Date.now()) {
      const start = task?.started_at_unix_ms;
      const end = task?.finished_at_unix_ms ?? (task?.live || task?.state === "running" ? now : null);
      if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || end < start) return "";
      const seconds = Math.floor((end - start) / 1000);
      return seconds >= 60 ? `${Math.floor(seconds / 60)}分${seconds % 60}秒` : `${seconds}秒`;
    }
    function relativeTime(ms, now = Date.now()) {
      if (!Number.isSafeInteger(ms)) return "时间未知";
      const seconds = Math.max(0, Math.floor((now - ms) / 1000));
      if (seconds < 60) return "刚刚";
      const formatter = new Intl.RelativeTimeFormat("zh-CN", { numeric: "auto" });
      if (seconds < 3600) return formatter.format(-Math.floor(seconds / 60), "minute");
      if (seconds < 86400) return formatter.format(-Math.floor(seconds / 3600), "hour");
      return formatter.format(-Math.floor(seconds / 86400), "day");
    }
    function TaskTime({ task }) {
      const [now, setNow] = useState(Date.now);
      const running = task.live || task.state === "running";
      useEffect(() => {
        let timer;
        const tick = () => { setNow(Date.now()); timer = setTimeout(tick, running ? 1000 : 60_000); };
        timer = setTimeout(tick, running ? 1000 : 60_000);
        return () => clearTimeout(timer);
      }, [running]);
      const created = task.created_at_unix_ms ?? task.started_at_unix_ms;
      return h("span", null, [
        duration(task, now), Number.isSafeInteger(created) ?
          `${task.created_at_source && task.created_at_source !== "adapter" ? "≈ " : ""}${relativeTime(created, now)}` : "",
      ].filter(Boolean).join(" · "));
    }

    /** One explicit operator action. Never replay terminal input on reconnect. */
    async function openDshTerminal(ctx, sessionId, selector, signal, fetcher = window.fetch.bind(window)) {
      const terminals = ctx.get("webTerminals");
      const sidebar = ctx.get("sidebarRight");
      if (!terminals || !sidebar?.openTabs || !sidebar?.mounted) throw new Error("DSH 终端接口不可用");
      const operation = new AbortController();
      const aborted = () => operation.abort(signal?.reason);
      signal?.addEventListener("abort", aborted, { once: true });
      const changed = () => {
        if (sidebar.mounted.getSnapshot() !== sessionId) operation.abort(new Error("会话已切换"));
      };
      const unwatch = sidebar.mounted.subscribe(changed);
      let allocated;
      try {
        signal?.throwIfAborted();
        changed();
        operation.signal.throwIfAborted();
        const response = await fetcher(`${ROUTE}?${new URLSearchParams({ session: sessionId, selector, view: "terminal" })}`, {
          signal: operation.signal, cache: "no-store", credentials: "same-origin", redirect: "error",
        });
        const prepared = await response.json();
        if (!response.ok) throw new Error(prepared.error || `HTTP ${response.status}`);
        const argv = prepared.argv;
        if (prepared.schema_version !== 1 || prepared.selector !== selector || prepared.terminal_status !== "ready" ||
            !Array.isArray(argv) || argv.length !== 6 || !argv.every((arg) => typeof arg === "string" && !arg.includes("\0")) ||
            !argv[0].startsWith("/") || argv[1] !== "exec" || argv[2] !== "--session" ||
            argv[3] !== "-it" || argv[4] !== selector || argv[5] !== "/bin/sh") throw new Error("实例终端参数无效");
        const choices = await terminals.launchShells(sessionId, operation.signal);
        // A verified Bash bootstrap gives one known quoting/exec contract,
        // independently of the user's remembered/default shell.
        const bash = choices.shells.find((shell) => /(?:^|\/)bash$/.test(shell.path));
        if (!bash) throw new Error("DSH 执行环境未提供 Bash，未打开终端");
        operation.signal.throwIfAborted();
        changed();
        operation.signal.throwIfAborted();
        const before = new Set(sidebar.openTabs.getSnapshot().map((tab) => `${tab.sessionId}:${tab.tabId}`));
        sidebar.openTab("terminal", { params: { shellPath: bash.path }, preferNewPane: true });
        const created = sidebar.openTabs.getSnapshot().filter((tab) =>
          tab.sessionId === sessionId && tab.kind === "terminal" && !before.has(`${tab.sessionId}:${tab.tabId}`));
        if (created.length !== 1) throw new Error("无法确认新终端，未发送命令");
        allocated = created[0];
        const view = terminals.view(sessionId, allocated.tabId, allocated.contentId, undefined, bash.path);
        await new Promise((resolve, reject) => {
          let settled = false;
          let unsubscribe = () => {};
          const deadline = setTimeout(() => done(new Error("终端连接超时")), 15_000);
          const abort = () => done(new Error("终端连接已取消"));
          function done(error) {
            if (settled) return;
            settled = true;
            clearTimeout(deadline);
            unsubscribe();
            operation.signal.removeEventListener("abort", abort);
            error ? reject(error) : resolve();
          }
          function check() {
            const state = view.state.getSnapshot();
            if (state.phase === "connected" && state.writable) done();
            else if (["failed", "closed", "closing"].includes(state.phase)) done(new Error(state.error || "终端不可用"));
          }
          unsubscribe = view.state.subscribe(check);
          if (settled) { unsubscribe(); return; }
          operation.signal.addEventListener("abort", abort, { once: true });
          if (operation.signal.aborted) abort(); else check();
        });
        operation.signal.throwIfAborted();
        const quote = (arg) => `'${arg.replace(/'/g, "'\\''")}'`;
        const command = `builtin exec -- ${argv.map(quote).join(" ")}; builtin exit\r`;
        if (new TextEncoder().encode(command).byteLength > 16 * 1024) throw new Error("终端命令过长");
        // This replaces the bootstrap shell. CLI failure exits the terminal;
        // it cannot silently fall back to an interactive Host prompt.
        view.write(command);
        void view.rename(`pVisor ${prepared.session_id || ""}`);
        return { status: "launch_sent", terminalId: view.id };
      } catch (error) {
        if (allocated) {
          terminals.close(sessionId, allocated.tabId, allocated.contentId);
          if (sidebar.mounted.getSnapshot() === sessionId) sidebar.close(allocated.tabId);
        }
        throw error;
      } finally {
        unwatch();
        signal?.removeEventListener("abort", aborted);
      }
    }

    /** One visible panel's sequential reader. Abort/generation guards reject stale replies. */
    function createMonitor({ sessionId, fetch: fetcher, publish, schedule = setTimeout, cancel = clearTimeout }) {
      let state = { sessionId, tasks: [], selected: null, view: "changes", cursor: null, previous: [],
        page: null, detail: null, error: null, busy: false };
      let visible = false;
      let generation = 0;
      let timer;
      let controller;
      let disposed = false;
      // Observation time alone is not a UI change. Retain object identity on
      // unchanged reads so background synchronization cannot repaint the list.
      const fingerprint = (value) => JSON.stringify(value, (key, item) => key === "observed_at" ? undefined : item);
      const emit = (patch) => {
        let changed = false;
        for (const key of Object.keys(patch)) {
          if (fingerprint(state[key]) === fingerprint(patch[key])) patch[key] = state[key];
          else changed = true;
        }
        if (!changed) return;
        state = { ...state, ...patch };
        publish(state);
      };
      let detailIdentity;
      function stop() {
        generation++;
        cancel(timer);
        controller?.abort();
      }
      async function get(params, signal) {
        // Document-relative route preserves DSH deployments under a URL prefix.
        const reply = await fetcher(`${ROUTE}?${new URLSearchParams({ session: sessionId, ...params })}`, {
          signal, cache: "no-store", credentials: "same-origin", redirect: "error",
        });
        if (!reply.ok) {
          const failure = await reply.json().catch(() => ({}));
          throw new Error(failure.error || `HTTP ${reply.status}`);
        }
        const value = await reply.json();
        if (value.schema_version !== 1 || typeof value.observed_at !== "string" ||
            typeof value.project !== "string") throw new Error("Unsupported monitor response.");
        return value;
      }
      async function refresh({ background = false } = {}) {
        stop();
        if (!visible || disposed) return;
        const current = generation;
        controller = new AbortController();
        const signal = controller.signal;
        if (!background) emit({ busy: true });
        try {
          const page = await get(state.cursor ? { cursor: state.cursor } : {}, signal);
          if (current !== generation || disposed) return;
          if (!Array.isArray(page.tasks)) throw new Error("Task inventory unavailable.");
          // New tasks can push the selected row onto another page. Keep its
          // exact selector; the server still validates project ownership.
          const selected = state.selected;
          emit({ page, tasks: page.tasks, error: null });
          const selectedTask = page.tasks.find((task) => task.selector === selected);
          const identity = fingerprint({ selected, view: state.view, task: selectedTask });
          if (selected && (!background || !state.detail || !selectedTask || identity !== detailIdentity)) {
            const detail = await get({ selector: selected, view: state.view }, signal);
            if (current !== generation || disposed) return;
            if (detail.selector !== selected || detail.project !== page.project) {
              throw new Error("Task detail identity mismatch.");
            }
            emit({ detail });
            detailIdentity = identity;
          }
        } catch (error) {
          if (current === generation && !disposed) emit({ error: error.message });
        } finally {
          if (current === generation && !disposed) {
            if (!background) emit({ busy: false });
            // No overlapping polls; a slow CLI read delays, rather than multiplies, requests.
            if (visible) {
              const active = state.tasks.some((task) => task.live || ["running", "unknown", "starting"].includes(task.state));
              timer = schedule(() => refresh({ background: true }), active ? 1000 : 2000);
            }
          }
        }
      }
      return {
        refresh,
        visible(value) {
          visible = value;
          if (value) void refresh({ background: !!state.page });
          else { stop(); emit({ busy: false }); }
        },
        select(selector) {
          if (!state.tasks.some((task) => task.selector === selector)) return;
          emit({ selected: selector, detail: null });
          void refresh();
        },
        deselect() {
          emit({ selected: null, detail: null });
          void refresh();
        },
        view(view) {
          if (!["changes", "diff", "output"].includes(view)) return;
          emit({ view, detail: null });
          void refresh();
        },
        next() {
          if (!state.page?.next_cursor) return;
          emit({ previous: [...state.previous, state.cursor], cursor: state.page.next_cursor,
            selected: null, detail: null, page: null, tasks: [] });
          void refresh();
        },
        previous() {
          if (!state.previous.length) return;
          const previous = [...state.previous];
          emit({ cursor: previous.pop(), previous, selected: null, detail: null, page: null, tasks: [] });
          void refresh();
        },
        dispose() { disposed = true; stop(); },
      };
    }

    function MonitorBody({ sessionId, useTabInfo, openTerminal }) {
      const { tab } = useTabInfo();
      const [state, setState] = useState(null);
      const [pageVisible, setPageVisible] = useState(document.visibilityState !== "hidden");
      const [showHistory, setShowHistory] = useState(false);
      const [selectedFile, setSelectedFile] = useState("");
      const [terminalError, setTerminalError] = useState("");
      const [terminalBusy, setTerminalBusy] = useState(false);
      const terminalGate = useMemo(() => ({ busy: false }), [sessionId]);
      const model = useMemo(() => createMonitor({
        sessionId, fetch: window.fetch.bind(window), publish: setState,
      }), [sessionId]);
      useEffect(() => {
        const changed = () => setPageVisible(document.visibilityState !== "hidden");
        document.addEventListener("visibilitychange", changed);
        return () => document.removeEventListener("visibilitychange", changed);
      }, []);
      useEffect(() => {
        model.visible(tab.visible && pageVisible && !tab.signal.aborted);
        const ended = () => model.visible(false);
        tab.signal.addEventListener("abort", ended, { once: true });
        return () => { tab.signal.removeEventListener("abort", ended); model.visible(false); };
      }, [model, tab.visible, tab.signal, pageVisible]);
      useEffect(() => tab.actions.bindCommands({ refresh: () => void model.refresh() }), [model, tab.actions]);
      const value = state?.sessionId === sessionId ? state : { tasks: [], previous: [] };
      const detail = value.detail;
      const button = (text, onClick, disabled = false) =>
        h(Button, { size: "sm", variant: "ghost", onClick, disabled }, text);
      const pre = (text) => h("pre", { style: { whiteSpace: "pre-wrap", overflowWrap: "anywhere",
        maxHeight: "45vh", overflow: "auto", margin: "8px 0", padding: 12,
        borderRadius: "var(--dsw-radius-md)", background: "var(--dsw-alias-button-tool-bar-fill)",
        fontSize: 12, lineHeight: 1.7 } }, text);
      const count = (n) => n == null ? "?" : String(n);
      const date = (ms) => Number.isSafeInteger(ms) ? new Date(ms).toLocaleString("zh-CN", {
        hour12: false, timeZoneName: "short",
      }) : "—";
      const history = (task) => ["discarded", "dropped", "applied"].includes(task.filesystem);
      const badge = (task) => {
        const [label, tone] = mainStatus(task);
        return h(Tag, { tone }, h("span", { "aria-hidden": true }, "●"), ` ${label}`);
      };
      const muted = { color: "var(--dsw-alias-label-tertiary)", fontSize: 12, lineHeight: 1.6 };
      const row = { display: "flex", alignItems: "center", flexWrap: "wrap", gap: 8 };
      const visibleTasks = value.tasks.filter((task) => showHistory || !history(task));
      const hidden = value.tasks.length - visibleTasks.length;
      const ended = detail && ["discarded", "dropped"].includes(detail.filesystem);
      const files = (detail?.files || detail?.changes?.map((file) => ({ ...file, diff_status: "unavailable" })) || []).slice(0, 200);
      const fileId = (file) => JSON.stringify([file.path, file.path_bytes]);
      const chosenFile = files.find((file) => fileId(file) === selectedFile) ||
        files.find((file) => file.diff_status === "text") || files[0];
      const pathLabel = (path) => path.replace(/[\x00-\x1f\x7f\u202a-\u202e\u2066-\u2069]/g,
        (character) => `\\u${character.charCodeAt(0).toString(16).padStart(4, "0")}`);
      const changeLabel = { added: "新增", modified: "修改", deleted: "删除", type_changed: "类型变化", opaque: "目录变更" };
      const diffLabel = { directory: "目录", binary: "二进制", partial: "输出已截断", unavailable: "文本差异不可用" };
      const lineCounts = (file) => file.diff_status === "text" && Number.isSafeInteger(file.additions) &&
        Number.isSafeInteger(file.deletions) ?
        h("span", { style: { ...row, whiteSpace: "nowrap", fontSize: 12 } },
          h("span", { style: { color: "var(--dsw-alias-state-success-primary)" } }, `+${file.additions}`),
          h("span", { style: { color: "var(--dsw-alias-state-error-primary)" } }, `−${file.deletions}`)) :
        h("span", { style: muted }, diffLabel[file.diff_status] || "文本差异不可用");
      return h("section", { "aria-label": "pVisor monitor", style: {
        padding: 16, height: "100%", boxSizing: "border-box", overflow: "auto",
        color: "var(--dsw-alias-label-primary)", font: "inherit", fontSize: 13,
      } },
      h("header", { style: { ...row, justifyContent: "space-between", marginBottom: 8 } },
        h("span", { style: row },
          h("strong", null, value.selected && detail ? taskTitle(detail) : value.selected ? "任务详情" : "运行记录"),
          h(Tag, { tone: "quiet" }, "只读")),
        button("刷新", () => void model.refresh(), value.busy)),
      value.page && h("details", { style: muted },
        h("summary", null, value.page.project.split(/[\\/]/).filter(Boolean).at(-1) || value.page.project),
        h("div", { style: { overflowWrap: "anywhere", marginTop: 8 } }, value.page.project),
        h("div", null, "按创建时间倒序 · 每页 10 项"),
        h("div", null, "已清理记录仅保留任务元数据与历史执行证据。")),
      value.error && h("p", { role: "alert" }, `读取失败：${value.error}。显示的是旧观测，不代表当前状态。`),
      !value.page && value.busy && h("p", { role: "status", style: muted }, "加载中…"),
      !value.selected && h(React.Fragment, null,
      h("nav", { "aria-label": "历史记录筛选", style: { ...row, margin: "12px 0" } },
        h(Pill, { active: !showHistory, onClick: () => setShowHistory(false) }, "待处理"),
        h(Pill, { active: showHistory, onClick: () => setShowHistory(true) }, "全部")),
      !value.busy && !value.error && value.page && !visibleTasks.length &&
        h("div", { style: { ...muted, padding: "24px 0", textAlign: "center" } },
          h("p", null, hidden ? "本页暂无待处理任务" : "暂无任务"),
          hidden > 0 && button("查看全部", () => setShowHistory(true))),
      h("div", { role: "list", "aria-label": "pVisor 任务" }, visibleTasks.map((task) =>
        h("div", { key: task.selector, role: "listitem", style: {
          borderBottom: "1px solid var(--dsw-alias-border-l3)", padding: "8px 0",
        } },
          h(Button, { variant: "ghost", title: task.selector, onClick: () => model.select(task.selector),
            style: { display: "block", textAlign: "left", width: "100%", height: "auto", padding: "10px 8px" } },
            h("div", { style: { ...row, justifyContent: "space-between", marginBottom: 6 } },
              h("strong", { style: { overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", flex: 1 } }, taskTitle(task)),
              badge(task)),
            h("div", { title: commandText(task), style: { ...muted, fontFamily: "monospace",
              overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" } }, commandText(task)),
            h("div", { style: { ...muted, ...row, marginTop: 6 } },
              h(TaskTime, { task }),
              task.execution?.selected && h("span", {
                title: task.execution.fallback ? task.execution.reason : undefined,
              }, task.execution.selected === "vm" ? "VM" : task.execution.fallback ? "Host 沙箱 · 已授权降级" : "Host 沙箱"),
              task.changed_files != null && h("span", null, `${task.changed_files} 项变更`),
              task.filesystem === "staged" && ["failed", "cancelled"].includes(task.state) &&
                h("span", null, "候选已保留"))),
          task.error && h("p", { role: "status", style: muted }, task.error)))),
      (value.previous.length > 0 || value.page?.next_cursor) &&
        h("nav", { "aria-label": "任务分页", style: { ...row, justifyContent: "center", margin: "12px 0" } },
          value.previous.length > 0 && button("上一页", () => model.previous(), value.busy),
          h("span", { style: muted }, `第 ${value.previous.length + 1} 页`),
          value.page?.next_cursor && button("下一页", () => model.next(), value.busy))),
      value.selected && h("article", null,
        button("‹ 返回任务列表", () => model.deselect()),
        detail?.exec_supported && typeof openTerminal === "function" && button(
          terminalBusy ? "正在打开…" : "打开实例终端", async () => {
            if (terminalGate.busy) return;
            terminalGate.busy = true; setTerminalBusy(true); setTerminalError("");
            try { await openTerminal(value.selected, tab.signal); }
            catch (error) { setTerminalError(error.message); }
            finally { terminalGate.busy = false; setTerminalBusy(false); }
          }, terminalBusy),
        terminalError && h("p", { role: "alert" }, `无法打开实例终端：${terminalError}`),
        detail && h(React.Fragment, null,
          h("div", { style: { ...row, justifyContent: "space-between", margin: "12px 0 6px" } },
            badge(detail)),
          h("div", { style: { ...muted, fontFamily: "monospace", overflow: "hidden",
            textOverflow: "ellipsis", whiteSpace: "nowrap" }, title: commandText(detail) }, commandText(detail)),
          h("div", { style: { ...muted, ...row, marginTop: 6 } }, h(TaskTime, { task: detail }),
            detail.execution?.selected && h("span", { title: detail.execution.reason ?? undefined },
              detail.execution.selected === "vm" ? "VM" : detail.execution.fallback ? "Host 沙箱 · 已授权降级" : "Host 沙箱"))),
        h("nav", { "aria-label": "任务详情", style: { ...row, margin: "16px 0" } },
          ...[["changes", "文件变更"], ["output", "终态输出"]].map(([view, label]) =>
            h(Pill, { key: view, active: value.view === view, onClick: () => model.view(view) }, label))),
        !detail ? h("p", null, value.busy ? "正在读取详情…" : "详情尚未取得。") :
          h(React.Fragment, null,
            detail.detail_status !== "complete" ?
              h("p", { role: "status", style: muted }, detail.live ? "任务运行中" : detail.error || "当前详情不可用") :
              value.view !== "output" ?
                h(React.Fragment, null,
                  detail.changes?.length === 0 && h("p", null, ended ? "候选已清理" : "没有剩余变更"),
                  detail.changes?.length > 200 && h("p", null, "仅显示前 200 个变更；计数来自完整原生清单。完整清单可通过 pvisor_review 获取。"),
                  h("div", { style: { display: "flex", flexWrap: "wrap", gap: 16, alignItems: "start" } },
                    h("div", { role: "list", "aria-label": "候选文件", style: { flex: "1 1 180px", minWidth: 0 } },
                      files.map((file) => h("div", { key: fileId(file), role: "listitem" },
                        h(Button, { variant: "ghost", "aria-pressed": chosenFile === file,
                          onClick: () => setSelectedFile(fileId(file)), style: { display: "block", textAlign: "left",
                            width: "100%", height: "auto", padding: "10px 8px" } },
                          h("div", { style: { fontFamily: "monospace", overflowWrap: "anywhere",
                            whiteSpace: "pre-wrap", marginBottom: 6 } }, pathLabel(file.path)),
                          h("div", { style: { ...row, justifyContent: "space-between" } },
                            h("span", { style: muted }, changeLabel[file.kind] || "变更"), lineCounts(file)))))),
                    chosenFile && h("div", { "aria-label": "文件差异", style: { flex: "2 1 300px", minWidth: 0, maxWidth: "100%" } },
                      h("div", { style: { ...row, justifyContent: "space-between", overflowWrap: "anywhere" } },
                        h("code", { style: { whiteSpace: "pre-wrap" } }, pathLabel(chosenFile.path)), lineCounts(chosenFile)),
                      chosenFile.diff?.split("\n").length > 2000 &&
                        h("p", { style: muted }, "仅显示前 2000 行"),
                      chosenFile.diff ? h("pre", { style: { margin: "10px 0", padding: 10, overflow: "auto", maxHeight: "60vh",
                        borderRadius: "var(--dsw-radius-md)", background: "var(--dsw-alias-button-tool-bar-fill)",
                        fontSize: 12, lineHeight: 1.7 } },
                        chosenFile.diff.split("\n").slice(0, 2000).map((line, index) => h("div", { key: index, style: {
                          minHeight: "1.7em", whiteSpace: "pre",
                          color: line.startsWith("+") && !line.startsWith("+++") ? "var(--dsw-alias-state-success-primary)" :
                            line.startsWith("-") && !line.startsWith("---") ? "var(--dsw-alias-state-error-primary)" :
                              "var(--dsw-alias-label-secondary)",
                        } }, line))) : h("p", { style: muted }, diffLabel[chosenFile.diff_status] || "文本差异不可用"))))
                : h(React.Fragment, null,
                  h("p", null, `历史执行：${detail.task_state} · exit：${count(detail.task_exit_code)}`),
                  h("strong", null, "stdout"), pre(detail.stdout || "（空）"),
                  h("strong", null, "stderr"), pre(detail.stderr || "（空）"),
                  detail.output_truncated && h("p", null, "捕获输出已截断，不是完整日志。"),
                  detail.lineage && h("details", null, h("summary", null, "原生 fork 来源"), pre(JSON.stringify(detail.lineage, null, 2))))),
        h("details", { style: { ...muted, marginTop: 16 } }, h("summary", null, "任务标识与边界"),
          pre(value.selected),
          h("div", null, `开始 ${date(detail?.started_at_unix_ms)} · 结束 ${date(detail?.finished_at_unix_ms)}`),
          h("div", null, `更新 ${detail?.observed_at || "未知"} · generation ${count(detail?.generation)}`),
          h("p", null, "清单与 Diff 分别观测，不是冻结快照或 apply 冲突检查。"),
          h("p", null, `网络：${detail?.network || "未知"}。终态任务不能直接附着 shell；进入候选需要新的隔离会话，不能用宿主终端代替。`))),
      );
    }

    function apply(ctx) {
      ctx.effect(() => ctx.sidebarRightTabs.register({
        id: TAB_ID, kind: "pvisor-monitor", priority: "extension",
        title: () => "pVisor",
        guide: [{ id: "monitor", order: 40, title: () => "pVisor",
          description: () => "只读查看任务、候选变更与终态输出" }],
      }), "pvisor: monitor tab");
      ctx.effect(() => ctx.slots.inject("sidebar.right.pane.tab", () =>
        ctx.slots.register({ name: "sidebar.right.pane.tab", key: TAB_ID,
          inject: (sessionId) => ({ openTerminal: (selector, signal) => openDshTerminal(ctx, sessionId, selector, signal) }),
        }, MonitorBody)),
      "pvisor: monitor body");
    }

    return { inject: ["slots", "sidebarRightTabs"], apply, createMonitor, openDshTerminal,
      presentation: { taskTitle, commandText, mainStatus, duration, relativeTime } };
  },
});
