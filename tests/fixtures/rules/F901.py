def f():
    raise NotImplemented  # ERR


def g():
    raise NotImplemented("x")  # ERR


def h():
    raise NotImplementedError
