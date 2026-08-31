#pragma once

#include "base.hpp"

class HeaderDerived : public Base {
};

struct HeaderTemplateDerived : protected virtual ns::Tpl<int> {
};

class HeaderQualifiedDerived : private ns::Tpl<long> {
};
