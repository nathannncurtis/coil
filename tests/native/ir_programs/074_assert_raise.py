assert True
assert 1 + 1 == 2
try:
    assert 1 == 2
except AssertionError as e:
    print("AssertionError", e.args)
try:
    assert 1 == 2, "with message"
except AssertionError as e:
    print("AssertionError", e)
try:
    assert isinstance("s", int), f"got {type('s').__name__}"
except AssertionError as e:
    print("AssertionError", e)
def check(n):
    if n < 0:
        raise ValueError("negative: " + str(n))
    return n
print(check(5))
try:
    check(-1)
except ValueError as e:
    print(e)
try:
    raise ValueError("a", "b")
except ValueError as e:
    print(len(e.args), e.args)
e2 = ValueError("x")
print(e2.args, str(e2), repr(e2))
e3 = ValueError()
print(e3.args, str(e3), repr(e3))
try:
    raise Exception("plain")
except Exception as e:
    print(type(e).__name__, e)
