def noop():
    pass

def outer():
    def inner():
        return 42
    return inner()

print(outer())
print(noop())
