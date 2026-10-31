def deep():
    raise ValueError("deep-cause")
def mid():
    try:
        deep()
    except ValueError as e:
        raise RuntimeError("wrapped") from e
try:
    mid()
except RuntimeError as e:
    print(type(e).__name__, e)
    print(type(e.__cause__).__name__, e.__cause__.args[0])
    print(e.__suppress_context__)
def implicit():
    try:
        raise ValueError("first")
    except ValueError:
        raise TypeError("second")
try:
    implicit()
except TypeError as e:
    print(type(e).__name__, e, type(e.__context__).__name__)
def suppress():
    try:
        raise ValueError("hidden")
    except ValueError:
        raise TypeError("shown") from None
try:
    suppress()
except TypeError as e:
    print(e, e.__suppress_context__, e.__cause__)
def reraise():
    try:
        raise ValueError("r")
    except ValueError:
        raise
try:
    reraise()
except ValueError as e:
    print("reraised", e)
def bare():
    try:
        raise ValueError("b")
    except ValueError:
        try:
            raise TypeError("inner-b")
        except TypeError:
            raise
try:
    bare()
except TypeError as e:
    print("bare", e)
try:
    try:
        raise ValueError
    finally:
        print("fin-before-raise")
except ValueError:
    print("after-fin")
