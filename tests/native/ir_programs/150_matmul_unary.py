class M:
    def __init__(self, rows):
        self.rows = rows
    def __matmul__(self, other):
        a, b = self.rows, other.rows
        n, m, p = len(a), len(b), len(b[0])
        out = [[0] * p for _ in range(n)]
        for i in range(n):
            for j in range(p):
                s = 0
                for k in range(m):
                    s += a[i][k] * b[k][j]
                out[i][j] = s
        return M(out)
    def __repr__(self):
        return repr(self.rows)
x = M([[1, 2], [3, 4]])
y = M([[5, 6], [7, 8]])
print(x @ y)
print(M([[1, 0], [0, 1]]) @ x)
v = -5
print(+v, -v, ~v, not v, ~~v)
b = True
print(not b, -b)
f = 2.5
print(+f, -f)
s = "ab"
print(not s, +1)
i = 7
print(-i, +i, ~i, -(-i))
