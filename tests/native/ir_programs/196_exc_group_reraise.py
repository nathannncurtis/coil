def worker(n):
    if n % 2:
        raise ValueError(f"odd {n}")
    raise KeyError(f"even {n}")
def run():
    raise ExceptionGroup("workers", [worker(1), worker(2)])
try:
    run()
except* ValueError as e:
    print("value", [str(x) for x in e.exceptions])
except* KeyError as e:
    print("key", [str(x) for x in e.exceptions])
def wrapped():
    try:
        raise ExceptionGroup("g", [ValueError(1), TypeError("t")])
    except* ValueError:
        print("v handled")
try:
    wrapped()
except ExceptionGroup as e:
    print("unhandled", [type(x).__name__ for x in e.exceptions], e.message)
def nested_groups():
    inner = ExceptionGroup("inner", [TypeError("i1")])
    return ExceptionGroup("outer", [inner, ValueError("v")])
try:
    raise nested_groups()
except* TypeError as e:
    print("t", e.exceptions[0].args[0])
except* ValueError as e:
    print("v", e.exceptions[0].args[0])
try:
    raise ExceptionGroup("e", [ExceptionGroup("f", [ValueError(1), ValueError(2)])])
except* ValueError as e:
    print("two", len(e.exceptions))
