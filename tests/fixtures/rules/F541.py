x = 1
a = f"no placeholders"  # ERR
b = f"has {x}"
c = "plain"
d = f"{{escaped}}"  # ERR
e = f"a" f"b{x}"
