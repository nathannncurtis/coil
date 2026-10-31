try:
    raise ExceptionGroup("eg", [ValueError("v"), TypeError("t")])
except* ValueError as e:
    print("star-value", e.exceptions[0].args[0])
except* TypeError as e:
    print("star-type", len(e.exceptions))
try:
    raise ExceptionGroup("eg2", [ValueError("v1"), ValueError("v2")])
except* ValueError as e:
    print("group-of", len(e.exceptions), [x.args[0] for x in e.exceptions])
def raiser(kind):
    raise ExceptionGroup("m", [kind("x"), ValueError("y")])
try:
    raiser(TypeError)
except* TypeError:
    print("first-clause")
except* ValueError:
    print("second-clause")
try:
    raise ExceptionGroup("nested", [ExceptionGroup("inner", [ValueError("deep")]), TypeError("t")])
except* ValueError as e:
    print("nested-value", e.message, e.exceptions[0].exceptions[0].args[0])
except* TypeError:
    print("nested-type")
try:
    raise ValueError("plain-not-group")
except* ValueError as e:
    print("plain-star", e.exceptions[0].args[0])
try:
    raise ExceptionGroup("mix", [ValueError(1), KeyError("k")])
except* ValueError:
    print("mix-value")
except* Exception as e:
    print("mix-rest", len(e.exceptions))
