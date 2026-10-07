#!/usr/bin/env python3
"""Render startup article figures from its archived benchmark (requires matplotlib)."""
from pathlib import Path

from evidence_tsv import load as load_evidence

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt

ROOT = Path(__file__).resolve().parents[2]
ASSETS = ROOT / 'docs/overrides/assets/benchmarks'
REPORT = load_evidence(ROOT / 'docs/src/assets/benchmarks/.data/startup-20261003.tsv')
PREVIEW = ROOT / 'target/startup-figures'
PREVIEW.mkdir(parents=True, exist_ok=True)
plt.rcParams.update({'font.family': 'DejaVu Sans', 'font.size': 11,
                     'axes.spines.top': False, 'axes.spines.right': False,
                     'axes.spines.left': False, 'axes.edgecolor': '#cbd5e1',
                     'text.color': '#1e293b', 'axes.labelcolor': '#475569',
                     'xtick.color': '#475569', 'ytick.color': '#475569',
                     'svg.fonttype': 'none'})
OFFICIAL, TRIMMED = '#94a3b8', '#0f766e'


def save(fig, name):
    fig.savefig(ASSETS / f'{name}.svg', bbox_inches='tight', facecolor='white')
    fig.savefig(PREVIEW / f'{name}.png', dpi=140, bbox_inches='tight', facecolor='white')
    plt.close(fig)


# Percentiles are separate statistics; bars are not confidence intervals.
shapes = ['1cpu-128', '2cpu-128', '4cpu-128', '2cpu-2048']
labels = ['1 vCPU / 128 MiB', '2 vCPU / 128 MiB', '4 vCPU / 128 MiB', '2 vCPU / 2 GiB']
fig, axes = plt.subplots(1, 3, figsize=(13, 4.8), sharey=True)
for ax, metric, limit in zip(axes, ['p50', 'p95', 'p99'], [155, 180, 460]):
    for offset, fw, color in [(-.18, 'official', OFFICIAL), (.18, 'trimmed', TRIMMED)]:
        values = [REPORT['summary'][f'{fw}-{s}']['ready_ms'][metric] for s in shapes]
        bars = ax.barh([i + offset for i in range(4)], values, height=.31, color=color, label=fw.title())
        ax.bar_label(bars, labels=[f'{v:.1f}' for v in values], padding=4, fontsize=9)
    ax.set(xlim=(0, limit), xlabel='Ready latency (ms)', title=metric.upper())
    ax.set_yticks(range(4), labels)
    ax.grid(axis='x', color='#e2e8f0', zorder=0)
    ax.set_axisbelow(True)
axes[0].invert_yaxis()
fig.legend(*axes[0].get_legend_handles_labels(), loc='upper center', ncol=2, frameon=False,
           bbox_to_anchor=(.5, .93))
fig.suptitle('New-VM readiness: official vs trimmed firmware', fontsize=16, fontweight='bold', y=1.02)
fig.text(.5, .01, '100 samples per case · warm host caches · panels use different x-axis ranges · P99 retains outliers',
         ha='center', fontsize=10, color='#64748b')
fig.tight_layout(rect=(0, .045, 1, .85))
save(fig, 'startup-firmware-latency')

# Phase means add exactly; medians generally do not.
profile = REPORT['profiles']['trimmed-2cpu-128']
phases = profile['phases']
labels = ['Parent load', 'Parent preparation', 'Runner load', 'Runner preparation',
          'VMM construction', 'Guest boot + init + output']
colors = ['#94a3b8', '#0f766e', '#cbd5e1', '#5b7c99', '#8b7cad', '#d97706']
fig, ax = plt.subplots(figsize=(11, 5.1))
left = 0
for i, ((key, stats), label, color) in enumerate(zip(phases.items(), labels, colors)):
    value = stats['mean']
    ax.barh(i, value, left=left, height=.58, color=color)
    ax.text(left + value + .9, i, f'{value:.2f} ms', va='center', fontsize=10)
    left += value
assert abs(left - profile['ready_ms']['mean']) < 1e-6
ax.barh(6, left, height=.58, color='#1e293b')
ax.text(left + .9, 6, f'{left:.2f} ms', va='center', fontsize=10, fontweight='bold')
ax.set_yticks(range(7), labels + ['Total observed Ready'])
ax.invert_yaxis()
ax.set(xlim=(0, 101), xlabel='Elapsed time from harness start (ms)')
ax.grid(axis='x', color='#e2e8f0'); ax.set_axisbelow(True)
ax.set_title('Where startup time goes', loc='left', fontsize=16, fontweight='bold', pad=22)
fig.text(.5, .01, 'Trimmed · 2 vCPU / 128 MiB · 20 separate diagnostic samples · phase means, not medians',
         ha='center', fontsize=10, color='#64748b')
fig.tight_layout(rect=(0, .035, 1, 1))
save(fig, 'startup-phase-waterfall')

# Historical controlled batches from the article; different resource shapes.
fig, ax = plt.subplots(figsize=(11, 4.1))
labels = ['Persistence\n2 vCPU / 2 GiB', 'Boot entropy\n2 vCPU / 128 MiB']
for offset, label, values, color in [(-.18, 'Before', [163.522, 129.270], OFFICIAL),
                                    (.18, 'After', [136.217, 82.268], TRIMMED)]:
    bars = ax.barh([i + offset for i in range(2)], values, height=.31, label=label, color=color)
    ax.bar_label(bars, labels=[f'{v:.2f}' for v in values], padding=5, fontsize=11)
ax.set_yticks(range(2), labels); ax.invert_yaxis()
ax.set(xlim=(0, 195), xlabel='Ready P50 (ms)')
ax.grid(axis='x', color='#e2e8f0'); ax.set_axisbelow(True)
ax.legend(loc='lower right', frameon=False)
ax.set_title('Retained optimizations: independent historical experiments',
             loc='left', fontsize=15, fontweight='bold', pad=20)
fig.text(.5, .01, '50 pairs per experiment · no diagnostic logs · different batches: do not add gains or link them as a timeline',
         ha='center', fontsize=10, color='#64748b')
fig.tight_layout(rect=(0, .06, 1, 1))
save(fig, 'startup-retained-optimizations')
print(f'Rendered 3 SVG figures to {ASSETS}; PNG previews to {PREVIEW}')
