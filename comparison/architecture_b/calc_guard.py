"""Input guard for the Architecture B calculator tool (comparison arm; see comparison/architecture_b/README.md).

The tool evaluates an expression the language model chose. A character whitelist already blocks names and calls (no
letters), so code execution is not possible, but the whitelist admitted exponent chains such as 9**9**9, which do not
finish in reasonable time and can exhaust the process. This guard removes exponentiation and bounds the length. It has no dependencies so it
can be tested without the agent stack.
"""
import re

MAX_EXPRESSION_CHARS = 200
_ALLOWED = re.compile(r"[0-9+\-*/(). \t]+")


def check_expression(expression):
    """None if the expression may be evaluated, otherwise the error text to return to the model."""
    if not isinstance(expression, str):
        return "Error: expression must be text."
    if len(expression) > MAX_EXPRESSION_CHARS:
        return "Error: expression is too long."
    if not _ALLOWED.fullmatch(expression):
        return "Error: expression contains disallowed characters."
    if "**" in expression:
        return "Error: exponentiation is not supported."
    return None
