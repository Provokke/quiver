def f():
    unused = 1  # ERR
    used = 2
    a, b = 1, 2
    _ = 3
    return used


def g():
    try:
        pass
    except Exception as e:  # ERR
        pass


def h():
    x = 1
    return locals()


module_level = 1
