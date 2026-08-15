import sys


def f():
    return undefined_name  # ERR


def g():
    return later_defined


later_defined = 1
print(sys, __file__, __name__, len([]))


class C:
    attr = 1

    def m(self):
        return attr  # ERR


try:
    maybe
except NameError:
    pass
[x for x in range(3)]
x  # ERR
