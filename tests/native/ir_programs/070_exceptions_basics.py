try:
    raise ValueError("boom")
except ValueError as e:
    print("caught", type(e).__name__, e, e.args)
try:
    raise TypeError("t1")
except (ValueError, TypeError) as e:
    print("tuple-caught", e)
def throws():
    raise KeyError("k")
try:
    throws()
except KeyError as e:
    print("from-call", e)
try:
    1 / 0
except ZeroDivisionError:
    print("no-binding")
try:
    raise ValueError
except ValueError as e:
    print("bare-class", e.args)
try:
    x = [1][5]
except IndexError as e:
    print("IndexError", e)
try:
    {}["a"]
except KeyError as e:
    print("KeyError", e)
try:
    None + 1
except TypeError as e:
    print("TypeError", e)
try:
    int("x")
except ValueError as e:
    print("ValueError", e)
try:
    undefined_name
except NameError as e:
    print("NameError", e)
class MyError(Exception):
    pass
try:
    raise MyError("custom", 2)
except MyError as e:
    print("custom", e.args, str(e))
try:
    raise MyError
except Exception as e:
    print("parent-caught", type(e).__name__)
