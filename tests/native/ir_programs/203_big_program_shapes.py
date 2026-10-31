def process(records):
    out = []
    for rec in records:
        name, *vals = rec
        total = sum(vals)
        avg = total / len(vals)
        grade = "A" if avg >= 90 else "B" if avg >= 80 else "C"
        out.append({"name": name, "total": total, "avg": avg, "grade": grade})
    return out
data = [("ada", 95, 92, 98), ("bob", 75, 80), ("cy", 60)]
for r in process(data):
    print(r["name"], r["total"], r["grade"])
def summarize(rows):
    by_grade = {}
    for row in rows:
        by_grade.setdefault(row["grade"], []).append(row["name"])
    return {k: sorted(v) for k, v in sorted(by_grade.items())}
print(summarize(process(data)))
def stats(xs):
    n = len(xs)
    mean = sum(xs) / n
    var = sum((x - mean) ** 2 for x in xs) / n
    return (round(mean, 2), round(var, 2))
print(stats([1, 2, 3, 4]))
def pairs_sorted_by_second(d):
    return sorted(d.items(), key=lambda kv: kv[1], reverse=True)
print(pairs_sorted_by_second({"a": 1, "b": 3, "c": 2}))
class RunningStats:
    def __init__(self):
        self.n = 0
        self.total = 0
    def add(self, v):
        self.n += 1
        self.total += v
        return self
    @property
    def mean(self):
        return self.total / self.n
rs = RunningStats().add(2).add(4)
print(rs.n, rs.mean)
