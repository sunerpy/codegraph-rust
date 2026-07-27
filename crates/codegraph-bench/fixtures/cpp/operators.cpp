struct Vec2 {
    int x;
    Vec2 operator+(const Vec2& o) const { return Vec2{x + o.x}; }
    Vec2 operator[](int i) const { return Vec2{x + i}; }
    int get() const { return x; }
};

Vec2 explicit_operator_call(const Vec2& a, const Vec2& b) {
    return a.operator+(b);
}

Vec2 explicit_subscript_call(const Vec2& a) {
    return a.operator[](3);
}

Vec2 explicit_pointer_operator_call(const Vec2* p, const Vec2& b) {
    return p->operator+(b);
}

int plain_member_call(const Vec2& a) {
    return a.get();
}
