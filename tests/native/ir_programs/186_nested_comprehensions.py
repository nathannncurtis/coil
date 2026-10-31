grid = [[1, 2, 3], [4, 5, 6]]
print([c for row in grid for c in row])
print([[c * 2 for c in row] for row in grid])
print({r: [c for c in row] for r, row in enumerate(grid)})
print({c for row in grid for c in row if c % 2 == 0})
print([(r, c) for r in range(2) for c in range(2) if r != c])
print([x for x in [y + 1 for y in range(3)]])
def fibs(n):
    a, b = 0, 1
    for _ in range(n):
        yield a
        a, b = b, a + b
print([f for f in fibs(8)])
print([f * 2 for f in fibs(5) if f % 2 == 0])
print(sum(c for row in grid for c in row))
print(max(len(row) for row in grid))
print(sorted({c for row in grid for c in row}))
print(list(c for row in grid for c in row if c > 1))
text = "abc"
print([s * 2 for s in text])
print([[i] * i for i in range(4)])
matrix_t = [[grid[r][c] for r in range(2)] for c in range(3)]
print(matrix_t)
