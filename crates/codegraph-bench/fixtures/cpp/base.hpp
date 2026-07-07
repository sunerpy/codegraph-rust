#pragma once

class Base {
public:
    void base_method() {}
};

class Plain {
public:
    void plain_method() {}
};

template <typename T>
class Container {
public:
    void hold() {}
};

namespace ns {
template <typename T>
class Tpl {
public:
    void wrap() {}
};
}
