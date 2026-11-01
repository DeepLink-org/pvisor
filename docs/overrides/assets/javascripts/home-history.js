/* Original SVG renderer for pVisor's stage/apply history. */
import { label } from './home-scenario.js';
export const escapeHTML = value => String(value).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
export const WINDOW = 68;
export function historySVG(state, { width = 500, left = 0, fit = false, selected = null, locale = 'en' } = {}) {
  const w = Math.max(300, width), h = 470, mainY = 235, laneH = 43;
  const span = fit ? Math.max(WINDOW, state.time + 8) : WINDOW;
  const start = fit ? 0 : left;
  const scale = (w - 100) / span;
  const x = t => 24 + (t - start) * scale;
  const y = j => mainY + j.lane * laneH;
  const t = (zh, en) => locale === 'zh' ? zh : en;
  const parts = [];
  const text = (px, py, content, cls = '', extra = '') => `<text x="${px}" y="${py}" class="${cls}" ${extra}>${escapeHTML(content)}</text>`;
  const title = content => `<title>${escapeHTML(content)}</title>`;
  const path = (d, color, cls = '', extra = '') => `<path d="${d}" stroke="${color}" class="${cls}" fill="none" ${extra}/>`;
  const spacing = [10, 20, 30, 60].find(n => n * scale >= 46) || 60;
  for (let tick = Math.ceil(start / spacing) * spacing; tick <= Math.min(start + span, state.time); tick += spacing) {
    parts.push(path(`M${x(tick)} 35V433`, '#20354d', 'pv-grid-line'));
    parts.push(text(x(tick), 22, `${Math.floor(tick / 60)}:${String(tick % 60).padStart(2, '0')}`, 'pv-axis'));
  }
  parts.push(`<rect x="0" y="${mainY - 15}" width="${w}" height="30" fill="#80dac00a"/>`);
  parts.push(path(`M${x(0)} ${mainY}H${x(state.time)}`, '#80dac0', 'pv-trunk'));
  parts.push(text(12, mainY - 25, t('你的工作区', 'your workspace'), 'pv-main-label'));
  const jobs = Object.values(state.jobs);
  const related = new Set(selected ? [selected, state.jobs[selected]?.parent].filter(Boolean) : []);
  if (selected) jobs.filter(j => j.parent === selected).forEach(j => related.add(j.id));
  for (const job of jobs) {
    const jy = y(job), sx = x(job.start);
    const parent = state.jobs[job.parent];
    const originY = parent ? y(parent) : mainY;
    const last = job.log.at(-1)?.t || job.start;
    const until = job.end === null ? state.time : Math.max(job.end, last);
    if (until < start - 12 || job.start > start + span) continue;
    const color = job.status === 'dropped' ? '#61748c' : job.color;
    const bend = Math.min(22, Math.max(3, (x(until) - sx) / 2));
    const opacity = !selected || related.has(job.id) ? 1 : .3;
    parts.push(`<g opacity="${opacity}">`);
    parts.push(path(`M${sx} ${originY}C${sx + bend} ${originY} ${sx + bend} ${jy} ${sx + bend * 2} ${jy}H${Math.max(sx + bend * 2, x(until))}`, color, 'pv-branch-line'));
    if (job.end !== null && until > job.end) parts.push(path(`M${x(job.end)} ${jy}H${x(until)}`, '#0d1b30', '', 'stroke-width="3" stroke-dasharray="3 4"'));
    for (const event of job.log) {
      const ex = x(event.t);
      if (ex < 8 || ex > w - 30 || ['start', 'fork', 'read'].includes(event.type)) continue;
      const fail = event.type === 'conflict' || (event.type === 'test' && event.passed < event.total);
      const ec = fail ? '#ffaba5' : color;
      parts.push(`<g data-pick="${job.id}">${title(`${job.code} · ${label(event.message, locale)}`)}`);
      if (event.type === 'apply') {
        parts.push(path(`M${ex} ${jy}C${ex + 18} ${jy} ${ex + 18} ${mainY} ${ex} ${mainY}`, color, 'pv-apply-link', 'stroke-dasharray="3 3"'));
        parts.push(`<rect x="${ex - 3}" y="${mainY - 6}" width="6" height="12" rx="1" fill="${color}"/>`);
      } else if (fail) parts.push(path(`M${ex - 4} ${jy - 4}l8 8m0 -8l-8 8`, ec, 'pv-failure-mark'));
      else if (event.type === 'drop') parts.push(`<circle cx="${ex}" cy="${jy}" r="4" fill="#0d1b30" stroke="${ec}"/>`);
      else parts.push(`<rect x="${ex - 2.5}" y="${jy - 2.5}" width="5" height="5" rx="1" fill="${ec}" transform="rotate(45 ${ex} ${jy})"/>`);
      parts.push('</g>');
    }
    const ex = Math.max(30, Math.min(w - 45, x(until) + 8));
    parts.push(`<g data-pick="${job.id}" class="pv-history-tag" role="button" tabindex="0" aria-label="${escapeHTML(job.code + ' ' + label(job.title, locale))}" aria-pressed="${selected === job.id}">${title(label(job.task, locale))}<rect x="${ex}" y="${jy - 10}" width="34" height="20" rx="4" fill="${color}"/>${text(ex + 17, jy + 4, job.code, 'pv-tag-text', 'text-anchor="middle"')}</g>`);
    if (job.end === null) parts.push(`<circle class="pv-live-head" cx="${x(state.time)}" cy="${jy}" r="5" fill="${color}"/>`);
    parts.push('</g>');
  }
  for (const event of state.history.filter(e => e.type === 'edit' || e.type === 'verify')) {
    const ex = x(event.t); if (ex < 6 || ex > w - 15) continue;
    const color = event.type === 'edit' ? '#dfb36f' : '#80dac0';
    parts.push(`<g>${title(label(event.message, locale))}<circle cx="${ex}" cy="${mainY}" r="5" fill="${color}" stroke="#0d1b30" stroke-width="2"/>${text(ex, mainY + 28, event.type === 'edit' ? t('你的修改', 'your edit') : '20/20', 'pv-axis', 'text-anchor="middle"')}</g>`);
  }
  const nowX = Math.min(w - 10, x(state.time));
  if (nowX >= 0 && state.time <= start + span) {
    parts.push(path(`M${nowX} 34V433`, '#a8cefa55', '', 'stroke-dasharray="2 5"'));
    parts.push(text(nowX, 451, t('现在', 'now'), 'pv-axis', 'text-anchor="middle"'));
  }
  return `<svg viewBox="0 0 ${w} ${h}" role="img" aria-label="${t('任务历史：私有尝试、用户修改和选择性接收', 'Job history: private attempts, your edits, and selective apply')}"><desc>${t('点击任务编号查看。菱形表示写入或测试，叉号表示失败或冲突，虚线回到主线表示接收文件。', 'Select a Job to inspect it. Diamonds mark edits or tests, crosses mark failure or conflict, and dashed returns accept files.')}</desc>${parts.join('')}</svg>`;
}
