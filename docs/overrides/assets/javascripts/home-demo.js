import { CHAPTERS, DURATION, EVENTS, INITIAL_FILES, INITIAL_TIME, JOBS, counters, label, stateAt } from './home-scenario.js';
import { escapeHTML as esc, historySVG, WINDOW } from './home-history.js';

function mountDemo(root) {
  const locale = root.dataset.locale;
  const tr = (zh, en) => locale === 'zh' ? zh : en;
  const $ = selector => root.querySelector(selector);
  const motion = matchMedia('(prefers-reduced-motion: reduce)');
  let time = INITIAL_TIME, state = stateAt(time), selected = null, chosenFile = null;
  let playing = false, speed = 1, frame = 0, previous = 0, drawn = -1, signature = '';
  let wantsPlayback = !motion.matches, inView = false;
  let following = true, historyLeft = 0, fit = false, drag = null;
  const clock = t => `${Math.floor(t / 60)}:${String(Math.floor(t % 60)).padStart(2, '0')}`;
  const status = j => ({ running: tr('执行中', 'running'), review: tr('待审查', 'review'), failed: tr('测试失败', 'test failed'), conflict: tr('接收冲突', 'conflict'), applied: tr('已接收', 'applied'), dropped: tr('已清理', 'cleared') })[j.status];
  const bot = (color, n) => {
    const rows = ['0010100', '0111110', n % 2 ? '1101011' : '1011101', '1111111', '0111110', '0100010'];
    return `<svg class="pv-agent-icon" viewBox="0 0 28 24" aria-hidden="true">${rows.flatMap((row, y) => [...row].map((v, x) => v === '1' ? `<rect x="${x * 4}" y="${y * 4}" width="4" height="4" fill="${color}"/>` : '')).join('')}</svg>`;
  };
  function preserveFocus(container, html) {
    const active = document.activeElement;
    const pick = container.contains(active) ? active?.dataset.pick : null;
    container.innerHTML = html;
    if (pick) container.querySelector(`[data-pick="${pick}"]`)?.focus({ preventScroll: true });
  }
  function renderHistory() {
    if (following) historyLeft = Math.max(0, time - WINDOW + 8);
    const graph = $('[data-history-canvas]');
    preserveFocus(graph, historySVG(state, { width: graph.clientWidth, left: historyLeft, fit, selected, locale }));
    const range = $('[data-history-range]');
    range.max = Math.max(0, time - WINDOW + 8); range.value = historyLeft;
    range.disabled = fit || Number(range.max) === 0;
    $('[data-action="fit"]').setAttribute('aria-pressed', String(fit));
    $('[data-action="follow"]').disabled = following && !fit;
  }
  function renderJobs() {
    preserveFocus($('[data-jobs]'), JOBS.map((info, n) => {
      const j = state.jobs[info.id];
      if (!j) return `<div class="pv-job-placeholder">${bot(info.color, n)}<span>${info.code} · ${esc(label(info.title, locale))}</span><small>${tr('等待前序任务', 'waiting')}</small></div>`;
      const pending = Object.keys(j.upper).length;
      const progress = j.end === null ? Math.min(90, 12 + (time - j.start) * 2) : 100;
      return `<button type="button" class="pv-sim-job" data-pick="${j.id}" data-state="${j.status}" aria-pressed="${selected === j.id}" style="--job-color:${j.color}">
        <span class="pv-job-top">${bot(j.color, n)}<strong>${j.code}</strong><span>${esc(j.agent)}</span></span>
        <span class="pv-job-name">${esc(label(j.title, locale))}</span>
        <span class="pv-job-status">${esc(status(j))}${j.tests ? ` · ${j.tests.passed}/${j.tests.total}` : ''}</span>
        <span class="pv-job-bottom">${pending} ${tr('暂存', 'staged')} · ${j.applied.length} ${tr('接收', 'kept')}</span>
        <span class="pv-job-progress"><i style="width:${progress}%"></i></span>
      </button>`;
    }).join(''));
  }
  function logHTML(entries) {
    return `<div class="pv-activity-log">${entries.map((e, i) => `<div class="pv-log-entry" data-kind="${e.type}"><span class="pv-log-time">${clock(e.t)} ${e.job ? state.jobs[e.job].code : 'you'}</span><span ${i === entries.length - 1 ? 'data-typing' : ''}>${esc(label(e.message, locale))}</span></div>`).join('')}</div>`;
  }
  function renderInspector() {
    const job = state.jobs[selected];
    $('[data-inspector-title]').textContent = job ? label(job.title, locale) : tr('协调记录', 'Coordination');
    $('[data-inspector-code]').textContent = job ? `${job.code} · ${job.agent}` : 'checkout';
    if (!job) {
      const significant = state.log.filter(e => !['read', 'write'].includes(e.type));
      $('[data-inspector]').innerHTML = `<p class="pv-panel-summary">${tr('先检查候选，再接收文件。工作区的变化始终留在主线上。', 'Review proposals before accepting files. Changes to your workspace stay on the main line.')}</p>${logHTML(significant.slice(-9))}<div class="pv-panel-hint">${bot('#81b8ff', 0)}${tr('点选一个任务，查看它的差异和执行记录。', 'Select a Job to inspect its diff and activity.')}</div>`;
      return;
    }
    // Keep accepted files available for inspection, with their original proposal.
    const writes = job.log.filter(e => e.type === 'write');
    const files = [...new Set([...Object.keys(job.upper), ...writes.map(e => e.path), ...job.applied])];
    if (!files.includes(chosenFile)) chosenFile = files[0] || null;
    const path = chosenFile;
    const proposed = path ? job.upper[path] ?? [...writes].reverse().find(e => e.path === path)?.content : null;
    const before = path ? job.baseline[path] : null;
    let diff = '';
    if (proposed !== null && proposed !== undefined) {
      const oldLines = (before || '').split('\n');
      const newLines = proposed.split('\n');
      diff = oldLines.filter(l => l && !newLines.includes(l)).map(l => `<span class="pv-diff-removed">− ${esc(l)}</span>`).join('');
      diff += newLines.map(l => `<span class="${oldLines.includes(l) ? 'pv-diff-context' : 'pv-diff-added'}">${oldLines.includes(l) ? '  ' : '+ '}${esc(l)}</span>`).join('');
    }
    const conflict = job.conflicts.includes(path);
    const disposition = job.applied.includes(path) ? tr('已接收', 'accepted') : job.status === 'dropped' ? tr('已丢弃 · 历史记录', 'discarded · history') : tr('候选差异', 'proposed diff');
    $('[data-inspector]').innerHTML = `<p class="pv-panel-summary">${esc(label(job.task, locale))}</p>
      ${files.length ? `<label class="pv-file-picker"><span>${tr('查看文件', 'Inspect file')}</span><select data-file>${files.map(p => `<option value="${esc(p)}" ${p === path ? 'selected' : ''}>${esc(p)}</option>`).join('')}</select></label>
      <div class="pv-diff-panel"><div class="pv-diff-label">${disposition}</div><pre><code>${diff}</code></pre></div>` : `<div class="pv-panel-empty">${tr('尚无候选修改', 'No proposed edits yet')}</div>`}
      ${conflict ? `<p class="pv-conflict-note">${tr('原像与当前文件不同，拒绝覆盖。你的说明仍在原项目里。', 'The baseline differs from the current file. Apply refuses to overwrite your note.')}</p>` : ''}
      ${job.tests ? `<p class="pv-test-badge" data-pass="${job.tests.passed === job.tests.total}">${job.tests.passed}/${job.tests.total} ${tr('示例测试', 'example tests')}</p>` : ''}
      <div class="pv-terminal-heading">${tr('任务记录', 'Job activity')} · Host</div>${logHTML(job.log.slice(-4))}`;
  }
  function renderProject() {
    const price = state.files['shop/pricing.py'];
    const note = price.includes('Amounts are in USD');
    const changed = Object.entries(state.files).filter(([p, c]) => INITIAL_FILES[p] !== c);
    $('[data-project]').innerHTML = `<div class="pv-project-heading"><strong>${tr('原项目', 'Your project')}</strong><span>${changed.length} ${tr('个文件变化', 'changed files')}</span></div>
      <div class="pv-project-check">${note ? '✓' : '·'} ${tr('用户的金额单位说明', 'Your currency note')}</div>
      <div class="pv-project-check">✓ ${tr('README 未提交草稿保留', 'Uncommitted README preserved')}</div>
      <div class="pv-project-check">${state.verification ? '✓ 20/20' : '◌'} ${state.verification ? tr('最终示例测试通过', 'final example tests pass') : tr('等待最终组合验证', 'final verification pending')}</div>`;
  }
  function render(force = false) {
    state = stateAt(time);
    const key = `${state.history.length}/${selected}/${chosenFile}`;
    if (force || key !== signature) {
      signature = key;
      const c = counters(state);
      $('[data-metrics]').innerHTML = [
        [c.running, tr('正在执行', 'running')], [c.staged, tr('候选文件', 'staged files')],
        [c.accepted, tr('已接收文件', 'accepted files')], [c.conflicts, tr('冲突拦截', 'conflict caught')],
      ].map(([n, name]) => `<div><strong>${n}</strong><span>${name}</span></div>`).join('');
      renderJobs(); renderProject(); renderInspector();
    }
    renderHistory();
    const chapter = CHAPTERS.findLastIndex(c => c.t <= time);
    $('[data-chapter-number]').textContent = String(chapter + 1).padStart(2, '0');
    $('[data-caption]').textContent = label(CHAPTERS[chapter].note, locale);
    root.querySelectorAll('[data-chapter]').forEach(b => b.setAttribute('aria-pressed', String(Number(b.dataset.chapter) === chapter)));
    $('[data-time-range]').value = time;
    $('[data-clock]').textContent = `${clock(time)} / ${clock(DURATION)}`;
    const recent = selected ? state.jobs[selected]?.log.at(-1) : state.log.filter(e => !['read', 'write'].includes(e.type)).at(-1);
    const typing = $('[data-typing]');
    if (typing && recent) {
      const full = label(recent.message, locale);
      typing.textContent = playing && !motion.matches ? full.slice(0, Math.max(1, Math.floor((time - recent.t) * 32))) : full;
    }
    $('[data-action="next"]').disabled = time >= DURATION;
    root.dataset.time = String(Math.floor(time));
    root.dataset.playing = String(playing);
  }
  function pause(manual = true) {
    if (manual) wantsPlayback = false;
    playing = false; cancelAnimationFrame(frame); previous = 0;
    $('[data-action="play"]').textContent = tr('播放', 'Play');
    root.dataset.playing = 'false';
    // A paused scene must expose complete text, including for reduced motion.
    signature = ''; render(true);
  }
  function tick(now) {
    if (!playing) return;
    if (previous) time = Math.min(DURATION, time + Math.min(now - previous, 200) / 700 * speed);
    previous = now;
    if (now - drawn > 120) { render(); drawn = now; }
    if (time >= DURATION) { pause(); return; }
    frame = requestAnimationFrame(tick);
  }
  function play() {
    wantsPlayback = true;
    if (playing) return;
    if (time >= DURATION) { time = 0; selected = null; }
    playing = true; previous = 0; following = true;
    $('[data-action="play"]').textContent = tr('暂停', 'Pause');
    root.dataset.playing = 'true';
    frame = requestAnimationFrame(tick);
  }
  function seek(next) {
    pause(); time = Math.max(0, Math.min(DURATION, next)); following = true;
    if (selected && !stateAt(time).jobs[selected]) selected = null;
    render(true);
    $('[data-announcement]').textContent = label(CHAPTERS.findLast(c => c.t <= time).note, locale);
  }
  function pick(id) { selected = selected === id ? null : id; chosenFile = null; render(true); }
  function click(e) {
    const picked = e.target.closest('[data-pick]');
    if (picked && !drag?.moved) { pick(picked.dataset.pick); return; }
    const chapter = e.target.closest('[data-chapter]');
    if (chapter) { seek(CHAPTERS[Number(chapter.dataset.chapter)].t); return; }
    switch (e.target.closest('[data-action]')?.dataset.action) {
      case 'play': playing ? pause() : play(); break;
      case 'reset': selected = null; chosenFile = null; fit = false; seek(0); break;
      case 'next': seek(EVENTS.find(e => e.t > time)?.t ?? DURATION); break;
      case 'overview': selected = null; chosenFile = null; render(true); break;
      case 'fit': fit = !fit; renderHistory(); break;
      case 'follow': following = true; fit = false; renderHistory(); break;
    }
  }
  function input(e) {
    if (e.target.matches('[data-time-range]')) seek(Number(e.target.value));
    if (e.target.matches('[data-history-range]')) { following = false; historyLeft = Number(e.target.value); renderHistory(); }
  }
  function change(e) {
    if (e.target.matches('[data-speed]')) speed = Number(e.target.value);
    if (e.target.matches('[data-file]')) { chosenFile = e.target.value; renderInspector(); $('[data-file]')?.focus({ preventScroll: true }); }
  }
  function keydown(e) {
    if (e.target.matches('.pv-history-tag') && ['Enter', ' '].includes(e.key)) { e.preventDefault(); pick(e.target.dataset.pick); }
  }
  const graph = $('[data-history-canvas]');
  function pointerDown(e) {
    if (e.button !== 0 || e.target.closest('[data-pick]') || fit) return;
    drag = { x: e.clientX, left: historyLeft, moved: false };
    graph.setPointerCapture(e.pointerId);
  }
  function pointerMove(e) {
    if (!drag) return;
    const delta = e.clientX - drag.x;
    if (Math.abs(delta) > 4) drag.moved = true;
    if (drag.moved) {
      following = false;
      historyLeft = Math.max(0, Math.min(Math.max(0, time - WINDOW + 8), drag.left - delta / Math.max(1, graph.clientWidth - 100) * WINDOW));
      renderHistory();
    }
  }
  function pointerUp() { drag = null; }
  const visibility = () => {
    if (wantsPlayback && inView && !document.hidden && time < DURATION) play();
    else if (playing) pause(false);
  };
  const suspend = () => pause(false);
  const motionChange = () => { if (motion.matches) pause(); };
  const resize = new ResizeObserver(() => renderHistory());
  const intersection = new IntersectionObserver(entries => {
    inView = entries[0].isIntersecting;
    visibility();
  });
  root.addEventListener('click', click); root.addEventListener('input', input);
  root.addEventListener('change', change); root.addEventListener('keydown', keydown);
  graph.addEventListener('pointerdown', pointerDown); graph.addEventListener('pointermove', pointerMove);
  graph.addEventListener('pointerup', pointerUp); graph.addEventListener('pointercancel', pointerUp);
  document.addEventListener('visibilitychange', visibility); window.addEventListener('pagehide', suspend);
  window.addEventListener('pageshow', visibility); motion.addEventListener('change', motionChange);
  $('[data-chapters]').innerHTML = CHAPTERS.map((c, i) => `<button type="button" data-chapter="${i}" aria-pressed="false"><span>0${i + 1}</span>${esc(label(c.title, locale))}</button>`).join('');
  $('[data-fallback]').hidden = true; $('.pv-simulation').hidden = false; $('.pv-demo-controls').hidden = false;
  root.dataset.ready = 'true'; render(true); resize.observe(graph); intersection.observe(root);
  return () => {
    playing = false; cancelAnimationFrame(frame); resize.disconnect(); intersection.disconnect();
    root.removeEventListener('click', click); root.removeEventListener('input', input); root.removeEventListener('change', change); root.removeEventListener('keydown', keydown);
    graph.removeEventListener('pointerdown', pointerDown); graph.removeEventListener('pointermove', pointerMove); graph.removeEventListener('pointerup', pointerUp); graph.removeEventListener('pointercancel', pointerUp);
    document.removeEventListener('visibilitychange', visibility); window.removeEventListener('pagehide', suspend);
    window.removeEventListener('pageshow', visibility); motion.removeEventListener('change', motionChange);
  };
}

function mountMechanisms() {
  const cards = [...document.querySelectorAll('.pv-idea-diagram')];
  const media = matchMedia('(prefers-reduced-motion: reduce)');
  const visible = new Set();
  const button = document.querySelector('[data-motion-toggle]');
  const zh = document.documentElement.lang.startsWith('zh');
  let paused = false;
  const update = () => {
    cards.forEach(card => card.classList.toggle('pv-animating', visible.has(card) && !document.hidden && !media.matches && !paused));
    if (button) {
      button.hidden = false; button.disabled = media.matches;
      button.setAttribute('aria-pressed', String(paused || media.matches));
      button.textContent = media.matches ? (zh ? '已减少动态' : 'Reduced motion') : paused ? (zh ? '播放动画' : 'Play motion') : (zh ? '暂停动画' : 'Pause motion');
    }
  };
  const toggle = () => { paused = !paused; update(); };
  button?.addEventListener('click', toggle);
  const observer = new IntersectionObserver(entries => { entries.forEach(e => e.isIntersecting ? visible.add(e.target) : visible.delete(e.target)); update(); });
  cards.forEach(card => observer.observe(card));
  media.addEventListener('change', update); document.addEventListener('visibilitychange', update);
  update();
  return () => { observer.disconnect(); button?.removeEventListener('click', toggle); media.removeEventListener('change', update); document.removeEventListener('visibilitychange', update); };
}
let cleanup = () => {};
function initialize() {
  cleanup();
  const root = document.querySelector('[data-pv-demo]');
  const demoCleanup = root ? mountDemo(root) : () => {};
  const mechanismCleanup = mountMechanisms();
  cleanup = () => { demoCleanup(); mechanismCleanup(); };
}
if (globalThis.document$?.subscribe) globalThis.document$.subscribe(initialize);
else initialize();
