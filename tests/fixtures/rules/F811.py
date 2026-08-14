import os
import os  # ERR

try:
    import json
except ImportError:
    json = None


def g():
    pass


def g():  # ERR
    pass


os, json
