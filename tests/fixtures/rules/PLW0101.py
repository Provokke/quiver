def f():
    return 1
    x = 2  # ERR


def g():
    for i in range(3):
        continue
        i  # ERR
    raise ValueError
