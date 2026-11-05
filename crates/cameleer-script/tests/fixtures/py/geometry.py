from .helpers import shout

PI = 3.14

def area(r):
    return PI * r * r

class Greeter:
    def __init__(self, name):
        self.name = name
    def greet(self):
        return shout('hello ' + self.name)

_private = 1
