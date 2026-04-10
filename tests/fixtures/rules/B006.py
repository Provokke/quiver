def f(a=[]):  # ERR
    return a


def g(a={}):  # ERR
    return a


def h(a=None, b=()):
    return a, b
