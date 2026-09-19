#!/usr/bin/env python3
"""Finite checks of pvisor-algebra.md's structural laws, not a backend proof.
Run: python3 docs/pvisor-algebra-check.py
Rust property/contract tests validate the implementation itself.
"""
from dataclasses import dataclass
from itertools import product


@dataclass(frozen=True)
class Expression:
    operation: tuple
    contexts: tuple = ()


def wrap(expression, contexts):
    return Expression(expression.operation, expression.contexts + contexts)


def chains():
    layers = (('vm', 'A'), ('remote', 'B'), ('overlay', 'C'))
    return [xs for n in range(3) for xs in product(layers, repeat=n)]


def check():
    expressions = [Expression(('fs.read', file, offset, length))
                   for file, offset, length in product(('a', 'b'), (0, 1), (0, 1))]
    contexts = chains()
    for expression in expressions:
        assert wrap(expression, ()) == expression
        for a, b in product(contexts, repeat=2):
            assert wrap(wrap(expression, a), b) == wrap(expression, a + b)
            assert wrap(expression, a).operation == expression.operation
        vm, remote = (('vm', 'A'),), (('remote', 'B'),)
        assert wrap(expression, vm + remote) != wrap(expression, remote + vm)
        # The original request is retained; evidence reproduces each suffix change.
        history = []
        current = expression
        for suffix in (vm, remote):
            after = wrap(current, suffix)
            history.append((current, suffix, after))
            current = after
        replayed = expression
        for before, suffix, after in history:
            assert replayed == before
            replayed = wrap(replayed, suffix)
            assert replayed == after
        assert replayed == current
        assert expression.contexts == ()
    print('PASS: finite wrapper identity/associativity, order, immutable request and rewrite reconstruction')


if __name__ == '__main__':
    check()
