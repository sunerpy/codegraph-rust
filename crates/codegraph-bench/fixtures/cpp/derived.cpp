#include "base.hpp"

class D : public Base {
public:
    void d_method() {}
};

class T : public Container<int> {
public:
    void t_method() {}
};

class Both : public Container<char>, public Plain {
public:
    void both_method() {}
};

struct S : Container<double> {
    void s_method() {}
};

class Q : public ns::Tpl<int> {
public:
    void q_method() {}
};
