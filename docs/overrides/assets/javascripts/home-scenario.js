/* A local, deterministic illustration. No executor, model, or network calls.
 * File observations, proposals and selected applies share one state model.
 * The narrative's test results are authored examples, not runtime measurements. */
export const DURATION = 150;
export const INITIAL_TIME = 28;
const pair = (zh, en) => ({ zh, en });
export const label = (value, locale) => typeof value === 'string' ? value : value[locale];

const priceA = 'def total(items):\n    return sum(p * n for p, n in items)';
const priceB = '# Amounts are in USD; keep the public API.\n' + priceA;
const priceC = 'from decimal import Decimal\n\ndef total(items):\n    value = sum(Decimal(str(p)) * n for p, n in items)\n    return value.quantize(Decimal("0.01"))';
const priceD = '# Amounts are in USD; keep the public API.\n' + priceC;
const inventory = 'def reserve(stock, count):\n    return stock - count';
const inventoryFix = 'def reserve(stock, count):\n    if count <= 0 or count > stock:\n        raise ValueError("invalid quantity")\n    return stock - count';
const coupon = 'def discount(subtotal, amount):\n    return subtotal - amount';
const couponFix = 'def discount(subtotal, amount):\n    return max(0, subtotal - amount)';
const order = 'def shipping(subtotal, discounted):\n    return 0 if discounted >= 50 else 5';
const orderFix = 'def shipping(subtotal, discounted):\n    # Free shipping uses the pre-discount subtotal.\n    return 0 if subtotal >= 50 else 5';
export const INITIAL_FILES = Object.freeze({
  'shop/pricing.py': priceA, 'shop/inventory.py': inventory,
  'shop/coupons.py': coupon, 'shop/order.py': order,
  'README.md': '# Shop\n\nLocal development notes (uncommitted).',
});
export const SESSIONS = [
  { id: 'price', code: 'P1', lane: -1, color: '#81b8ff', title: pair('金额精度', 'Money precision'), agent: 'Ada', task: pair('用十进制定点计算，保留接口。', 'Use decimal arithmetic; keep the API.') },
  { id: 'stock', code: 'I1', lane: 1, color: '#80dac0', title: pair('库存边界', 'Stock limits'), agent: 'Lin', task: pair('拒绝超卖与负数数量。', 'Reject overselling and negative quantities.') },
  { id: 'coupon', code: 'C1', lane: -2, color: '#c1a0f5', title: pair('优惠券', 'Coupons'), agent: 'Sam', task: pair('折扣不能让金额小于零。', 'Keep discounted totals nonnegative.') },
  { id: 'round', code: 'P2', lane: -3, color: '#dfb36f', title: pair('另一种舍入', 'Try rounding'), agent: 'Kai', parent: 'price', task: pair('从已停止任务的文件状态尝试 round。', 'Try round from a stopped Session’s file checkpoint.') },
  { id: 'retry', code: 'P3', lane: 2, color: '#79c9eb', title: pair('保留用户修改', 'Keep your edit'), agent: 'Ada', task: pair('读当前项目，重做金额修复。', 'Redo the money fix from the current project.') },
  { id: 'integration', code: 'T1', lane: 3, color: '#f1a38e', title: pair('集成验证', 'Integration'), agent: 'Jo', task: pair('检查金额、优惠券与运费一起工作。', 'Check totals, discounts, and shipping together.') },
  { id: 'shipping', code: 'S1', lane: 4, color: '#c1a0f5', title: pair('运费规则', 'Shipping rule'), agent: 'Lin', task: pair('满额包邮使用折扣前金额。', 'Use the pre-discount total for free shipping.') },
  { id: 'docs', code: 'D1', lane: -4, color: '#80dac0', title: pair('补齐交付说明', 'Delivery notes'), agent: 'Sam', task: pair('记录边界用例，保留 README 草稿。', 'Document edge cases; preserve the README draft.') },
];
export const CHAPTERS = [
  { t: 0, title: pair('开始', 'Start'), note: pair('订单服务有三个问题。分别开任务，读现有项目，修改先留在各自的暂存区。', 'Three issues in the order service. Start separate Sessions against the existing project; retain their edits in private stages.') },
  { t: 18, title: pair('并行尝试', 'Explore'), note: pair('金额、库存、优惠券同时推进。Agent 在各自的文件视图里修改和测试，原项目继续可用。', 'Money, stock, and coupons progress together. Each agent edits and tests its own file view while the original project stays available.') },
  { t: 44, title: pair('遇到冲突', 'Conflict'), note: pair('你补了金额单位说明。旧修复基于更早的文件，接收时被拒绝；候选仍留着，可以检查或重试。', 'You added a currency note. The old fix used an earlier file, so apply refuses to overwrite it. The proposal remains available for review or retry.') },
  { t: 64, title: pair('选择接收', 'Select'), note: pair('先接收库存和优惠券的源码与测试。实验笔记留在暂存区，金额修复从你的新版本重新开始。', 'Accept stock and coupon source and tests. Leave experiment notes staged while the money fix starts again from your edit.') },
  { t: 96, title: pair('一起验证', 'Integrate'), note: pair('单独的修复都通过了示例测试。放在一起，折扣却破坏了满额包邮规则，还需要一次修复。', 'The individual fixes pass the example tests. Together, discounts break the free-shipping rule. One more fix is needed.') },
  { t: 138, title: pair('完成交付', 'Deliver'), note: pair('接收运费修复与说明，再验证最终项目。用户的修改和 README 草稿都保留，剩余候选随后清理。', 'Accept the shipping fix and notes, then verify the final project. Your edit and README draft remain; discard leftover proposals afterward.') },
];

const events = [];
const add = (t, type, session, data = {}) => events.push({ t, type, session, ...data });
const start = (t, session) => add(t, 'start', session);
const read = (t, session, paths) => add(t, 'read', session, { paths });
const write = (t, session, path, content) => add(t, 'write', session, { path, content });
const test = (t, session, passed, total, detail) => add(t, 'test', session, { passed, total, detail });
const stop = (t, session) => add(t, 'stop', session);
const apply = (t, session, paths) => add(t, 'apply', session, { paths });
const drop = (t, session) => add(t, 'drop', session);
start(3, 'price'); read(5, 'price', ['shop/pricing.py', 'tests/test_pricing.py']);
start(7, 'stock'); read(9, 'stock', ['shop/inventory.py', 'tests/test_inventory.py']);
start(11, 'coupon'); read(13, 'coupon', ['shop/coupons.py', 'tests/test_coupons.py']);
write(17, 'price', 'shop/pricing.py', priceC);
write(20, 'stock', 'shop/inventory.py', inventoryFix);
write(23, 'coupon', 'shop/coupons.py', couponFix);
write(26, 'price', 'tests/test_pricing.py', 'def test_decimal_total():\n    assert total([(0.1, 3)]) == Decimal("0.30")');
add(30, 'edit', null, { path: 'shop/pricing.py', content: priceB });
write(32, 'stock', 'tests/test_inventory.py', 'def test_no_overselling():\n    with raises(ValueError): reserve(3, 4)');
test(34, 'price', 8, 8, 'pricing');
write(35, 'coupon', 'tests/test_coupons.py', 'def test_discount_floor():\n    assert discount(10, 20) == 0');
write(36, 'price', 'rounding-notes.txt', 'Compare Decimal with round before delivery.');
stop(38, 'price');
add(40, 'fork', 'round', { parent: 'price' });
test(41, 'stock', 6, 6, 'inventory');
apply(44, 'price', ['shop/pricing.py', 'tests/test_pricing.py']);
stop(45, 'stock'); start(46, 'retry');
read(47, 'retry', ['shop/pricing.py', 'tests/test_pricing.py']);
write(48, 'coupon', 'coupon-notes.txt', 'Also check interaction with free shipping.');
test(49, 'coupon', 5, 5, 'coupons'); stop(50, 'coupon');
apply(52, 'stock', ['shop/inventory.py', 'tests/test_inventory.py']);
write(54, 'round', 'shop/pricing.py', 'def total(items):\n    return round(sum(p * n for p, n in items), 2)');
write(57, 'retry', 'shop/pricing.py', priceD);
apply(60, 'coupon', ['shop/coupons.py', 'tests/test_coupons.py']);
test(63, 'round', 7, 8, 'round(2.675, 2) = 2.67');
write(64, 'retry', 'tests/test_pricing.py', 'def test_decimal_total():\n    assert total([(0.1, 3)]) == Decimal("0.30")');
stop(66, 'round'); drop(69, 'round');
test(70, 'retry', 8, 8, 'pricing'); stop(72, 'retry');
apply(78, 'retry', ['shop/pricing.py', 'tests/test_pricing.py']);
start(82, 'integration'); read(84, 'integration', ['shop/pricing.py', 'shop/coupons.py', 'shop/order.py']);
test(92, 'integration', 19, 20, '$50 − $10 coupon: shipping should be $0, got $5');
stop(94, 'integration'); start(98, 'shipping');
read(100, 'shipping', ['shop/order.py', 'tests/test_checkout.py']);
start(104, 'docs'); read(106, 'docs', ['README.md', 'docs/checkout.md']);
write(108, 'shipping', 'shop/order.py', orderFix);
write(112, 'docs', 'docs/checkout.md', '# Checkout\n\nUse decimal totals. Reject invalid quantities.\nClamp discounts at zero. Free shipping uses the original subtotal.');
write(114, 'shipping', 'tests/test_checkout.py', 'def test_free_shipping_with_coupon():\n    assert shipping(50, discount(50, 10)) == 0');
write(116, 'docs', 'delivery-notes.txt', 'README draft checked; no edits required.');
test(118, 'docs', 3, 3, 'documentation checks'); stop(120, 'docs');
test(122, 'shipping', 20, 20, 'checkout'); stop(124, 'shipping');
apply(130, 'shipping', ['shop/order.py', 'tests/test_checkout.py']);
apply(136, 'docs', ['docs/checkout.md']);
add(140, 'verify', null, { passed: 20, total: 20 });
drop(143, 'price'); drop(145, 'coupon'); drop(147, 'docs');
export const EVENTS = events.sort((a, b) => a.t - b.t);

function newSession(id, t) {
  return { ...SESSIONS.find(j => j.id === id), start: t, end: null, status: 'running', baseline: {}, upper: {}, applied: [], tests: null, log: [], conflicts: [] };
}
export function createState() {
  return { time: 0, files: { ...INITIAL_FILES }, sessions: {}, history: [], log: [], applied: [], conflicts: [], verification: null };
}
// A null observation records absence, so a later same-name file is a conflict.
function observe(state, session, path) {
  if (!Object.hasOwn(session.baseline, path)) session.baseline[path] = state.files[path] ?? null;
}
export function reduceEvent(state, event) {
  const { t, type, session: id } = event;
  let session = state.sessions[id];
  let message;
  let paths = event.paths || (event.path ? [event.path] : []);
  let outcome = type;
  if (type === 'start' || type === 'fork') {
    session = state.sessions[id] = newSession(id, t);
    if (type === 'fork') {
      const parent = state.sessions[event.parent];
      if (!parent || parent.end === null) throw new Error('File fork requires a stopped parent');
      session.baseline = { ...parent.baseline }; session.upper = { ...parent.upper };
    }
    message = type === 'fork' ? pair('从 P1 文件检查点启动 P2', 'Start P2 from P1’s file checkpoint') : pair('启动 ' + session.code + ' · 私有暂存区', 'Start ' + session.code + ' · private stage');
  } else if (type === 'read') {
    paths.forEach(path => observe(state, session, path));
    message = pair('读取 ' + paths[0] + ' · 记录原像', 'Read ' + paths[0] + ' · record baseline');
  } else if (type === 'write') {
    observe(state, session, event.path); session.upper[event.path] = event.content;
    message = pair('写入 upper · ' + event.path, 'Write upper · ' + event.path);
  } else if (type === 'edit') {
    state.files[event.path] = event.content;
    message = pair('你修改了金额单位说明 · 原项目', 'You add a currency note · original project');
  } else if (type === 'test') {
    session.tests = { passed: event.passed, total: event.total, detail: event.detail };
    message = pair(`示例测试 ${event.passed}/${event.total} · ${event.detail}`, `Example tests ${event.passed}/${event.total} · ${event.detail}`);
  } else if (type === 'stop') {
    session.end = t; session.status = session.tests && session.tests.passed < session.tests.total ? 'failed' : 'review';
    message = pair('任务停止 · 候选文件可审查', 'Session stopped · proposal ready for review');
  } else if (type === 'apply') {
    if (session.end === null) throw new Error('Stop writers before apply');
    if (paths.some(p => !Object.hasOwn(session.upper, p))) throw new Error('Apply selects existing proposals');
    const conflicts = paths.filter(p => (state.files[p] ?? null) !== session.baseline[p]);
    if (conflicts.length) {
      session.status = 'conflict'; session.conflicts = conflicts; outcome = 'conflict';
      state.conflicts.push({ session: id, t, paths: conflicts });
      message = pair('拒绝覆盖 · ' + conflicts.join(', '), 'Apply refused · ' + conflicts.join(', '));
    } else {
      for (const p of paths) { state.files[p] = session.upper[p]; delete session.upper[p]; session.applied.push(p); }
      session.status = 'applied'; state.applied.push({ session: id, t, paths: [...paths] });
      message = pair(`审查后接收 ${paths.length} 个文件 · ${session.code}`, `Accept ${paths.length} reviewed files · ${session.code}`);
    }
  } else if (type === 'drop') {
    if (session.end === null) throw new Error('Stop before discarding proposals');
    paths = Object.keys(session.upper); session.upper = {}; session.status = 'dropped';
    message = pair(`丢弃剩余 ${paths.length} 个候选 · ${session.code}`, `Drop ${paths.length} remaining proposals · ${session.code}`);
  } else if (type === 'verify') {
    state.verification = { passed: event.passed, total: event.total };
    message = pair('最终项目 · 20/20 示例测试通过', 'Final project · 20/20 example tests pass');
  } else throw new Error('Unknown demo event: ' + type);
  const record = { ...event, type: outcome, paths, message };
  state.history.push(record); state.log.push(record); if (session) session.log.push(record);
  state.time = t;
  return state;
}
export function stateAt(time) {
  const state = createState();
  for (const event of EVENTS) { if (event.t > time) break; reduceEvent(state, event); }
  state.time = Math.max(0, Math.min(DURATION, time));
  return state;
}
export function counters(state) {
  const sessions = Object.values(state.sessions);
  return {
    running: sessions.filter(j => j.end === null).length,
    staged: sessions.reduce((n, j) => n + Object.keys(j.upper).length, 0),
    accepted: new Set(state.applied.flatMap(a => a.paths)).size,
    conflicts: state.conflicts.length,
  };
}
