// These assertions validate the explanatory browser model, not pVisor execution.
import test from 'node:test';
import assert from 'node:assert/strict';
import { EVENTS, INITIAL_FILES, counters, createState, reduceEvent, stateAt } from '../docs/overrides/assets/javascripts/home-scenario.js';
import { historySVG } from '../docs/overrides/assets/javascripts/home-history.js';

test('a proposal stays private; apply compares the observed file to the current target', () => {
  const before = stateAt(29);
  assert.equal(before.files['shop/pricing.py'], INITIAL_FILES['shop/pricing.py']);
  assert.match(before.sessions.price.upper['shop/pricing.py'], /Decimal/);
  const conflict = stateAt(44);
  assert.equal(conflict.sessions.price.status, 'conflict');
  assert.match(conflict.files['shop/pricing.py'], /Amounts are in USD/);
  assert.doesNotMatch(conflict.files['shop/pricing.py'], /Decimal/);
  assert.equal(conflict.files['tests/test_pricing.py'], undefined);
  assert.ok(conflict.sessions.price.upper['tests/test_pricing.py']);
  // Removing the intervening edit removes the conflict: it is not a timed label.
  const unchanged = createState();
  EVENTS.filter(e => e.t <= 44 && e.type !== 'edit').forEach(e => reduceEvent(unchanged, e));
  assert.equal(unchanged.sessions.price.status, 'applied');
  assert.match(unchanged.files['shop/pricing.py'], /Decimal/);
});

test('absence is a baseline and a newly created same-name file refuses the whole selection', () => {
  const s = stateAt(51);
  s.files['tests/test_inventory.py'] = 'user-created test';
  reduceEvent(s, EVENTS.find(e => e.t === 52));
  assert.equal(s.sessions.stock.status, 'conflict');
  assert.equal(s.files['shop/inventory.py'], INITIAL_FILES['shop/inventory.py']);
  assert.equal(s.files['tests/test_inventory.py'], 'user-created test');
});

test('a stopped file fork keeps its own proposal and original observations', () => {
  const s = stateAt(54);
  assert.deepEqual(s.sessions.round.baseline, s.sessions.price.baseline);
  assert.notEqual(s.sessions.round.upper, s.sessions.price.upper);
  assert.match(s.sessions.round.upper['shop/pricing.py'], /round/);
  assert.match(s.sessions.price.upper['shop/pricing.py'], /Decimal/);
  const running = stateAt(37);
  assert.throws(() => reduceEvent(running, { t: 37, type: 'fork', session: 'round', parent: 'price' }), /stopped/);
  assert.throws(() => reduceEvent(stateAt(35), { t: 35, type: 'apply', session: 'price', paths: ['shop/pricing.py'] }), /Stop writers/);
});

test('selective apply leaves notes; drop does not undo accepted files or user edits', () => {
  const partial = stateAt(60);
  assert.deepEqual(Object.keys(partial.sessions.coupon.upper), ['coupon-notes.txt']);
  assert.match(partial.files['shop/coupons.py'], /max/);
  assert.equal(partial.files['coupon-notes.txt'], undefined);
  const final = stateAt(150);
  assert.deepEqual(counters(final), { running: 0, staged: 0, accepted: 9, conflicts: 1 });
  assert.match(final.files['shop/pricing.py'], /Amounts are in USD/);
  assert.match(final.files['shop/pricing.py'], /Decimal/);
  assert.equal(final.files['README.md'], INITIAL_FILES['README.md']);
  assert.ok(Object.keys(final.files).every(p => !p.endsWith('notes.txt')));
  assert.match(final.files['shop/order.py'], /subtotal >= 50/);
  assert.equal(final.sessions.integration.tests.passed, 19);
  assert.equal(final.verification.passed, 20);
});

test('scrubbing reconstructs independent state and history remains readable in both locales', () => {
  const first = stateAt(28);
  stateAt(150).files['README.md'] = 'mutated';
  assert.deepEqual(stateAt(28), first);
  for (const locale of ['zh', 'en']) {
    for (const t of [0, 44, 96, 150]) {
      const svg = historySVG(stateAt(t), { locale, width: 320, fit: true });
      assert.ok(!svg.includes('NaN'));
      assert.match(svg, /<desc>/);
      if (t >= 44) assert.match(svg, /data-pick="price"/);
    }
  }
});
