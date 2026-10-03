#!/usr/bin/env python3
"""Reproduce the first product benchmark reports on a Linux host."""
import argparse
from pathlib import Path

from v1.common import Context
from v1 import filesystem, oci, network, apply, density, agent, isolation


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--binary',type=Path,default=Path('target/release/pvisor'))
    parser.add_argument('--firmware',type=Path,required=True)
    parser.add_argument('--samples',type=int,default=30)
    parser.add_argument('--warmups',type=int,default=3)
    parser.add_argument('--suites',default='filesystem')
    args=parser.parse_args()
    if args.samples<1 or args.warmups<0:
        parser.error('samples must be positive and warmups nonnegative')
    ctx=Context(args)
    oci.prepare(ctx)
    suites={'filesystem':filesystem.run,'network':network.run,'apply':apply.run,'density':density.run,'agent':agent.run,'isolation':isolation.run}
    for name in args.suites.split(','):
        if name not in suites:
            parser.error(f'unknown suite: {name}')
        suites[name](ctx)
    ctx.save()
    print(f'report: {ctx.output}/report.json')


if __name__=='__main__':
    main()
