struct AggregateOnly {
    int value;
};

class WithConstructor {
public:
    WithConstructor() {}
    explicit WithConstructor(int value) {}
};

class Defaults {
public:
    Defaults() {}
    explicit Defaults(int value = 0) {}
};

namespace left {
struct Widget {
    Widget() {}
};
}

namespace right {
struct Widget {
    Widget() {}
};
}

void aggregate_only() {
    AggregateOnly value{};
}

void constructor_default() {
    WithConstructor value;
}

void constructor_braced() {
    WithConstructor value{};
}

void constructor_value() {
    WithConstructor value(1);
}

void constructor_ambiguous_overload() {
    Defaults value;
}

void constructor_ambiguous_owner() {
    Widget value;
}

void constructor_explicit_owner() {
    left::Widget value;
}
