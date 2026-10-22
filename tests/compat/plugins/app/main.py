import importlib
from importlib.metadata import entry_points

found = {ep.name: ep.value for ep in entry_points(group="coil_compat.plugins")}
assert sorted(found) == ["alpha", "beta"], found
module = importlib.import_module(found["beta"])
assert module.run() == "beta ran"
assert importlib.import_module(found["alpha"]).NAME == "alpha"
print("COMPAT_OK plugins", sorted(found))
