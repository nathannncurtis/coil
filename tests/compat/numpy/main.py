import numpy as np

a = np.arange(6, dtype=np.float64).reshape(2, 3)
b = np.ones((3, 2))
c = a @ b
assert c.tolist() == [[3.0, 3.0], [12.0, 12.0]], c
print("COMPAT_OK numpy", np.__version__)
