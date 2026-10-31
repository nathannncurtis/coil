import typing
print("typing", typing.__name__)
import collections
print("collections", collections.__name__)
from collections import abc
print("abc", abc.__name__)
from collections.abc import Sequence
print("Sequence", Sequence.__name__)
import typing as t
print("alias", t is typing)
from typing import List
print("from-import ok")
def scope():
    import collections
    return collections
print("in-function", scope() is collections)
try:
    import nosuchmodule
except ModuleNotFoundError as e:
    print("ModuleNotFoundError", e)
try:
    from nosuchmodule import thing
except ImportError as e:
    print("ImportError", e)
try:
    from . import sibling
except ImportError as e:
    print("relative", e)
