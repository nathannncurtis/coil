def f():
    raise ExceptionGroup("g", [ValueError("v"), TypeError("t"), KeyError("k")])
try:
    f()
except* ValueError as e:
    print("v", [type(x).__name__ for x in e.exceptions])
except* (TypeError, KeyError) as e:
    print("tk", [type(x).__name__ for x in e.exceptions])
try:
    try:
        raise ExceptionGroup("g", [ValueError(1), TypeError(2)])
    except* ValueError:
        print("handled v")
except* TypeError:
    print("handled t")
def all_caught():
    out = "none"
    try:
        raise ExceptionGroup("g", [ValueError(1)])
    except* ValueError:
        out = "all"
    return out
print(all_caught())
def partial():
    try:
        raise ExceptionGroup("g", [ValueError(1), KeyError(2)])
    except* ValueError:
        print("v only")
try:
    partial()
except ExceptionGroup as e:
    print("leftover", [type(x).__name__ for x in e.exceptions])
try:
    raise ExceptionGroup("m", [ExceptionGroup("n", [ValueError("d")])])
except* ValueError as e:
    print("derived", e.exceptions[0].exceptions[0].args[0])
