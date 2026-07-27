#include "namespaced_member.hpp"

namespace simulator {
int ManifestStartup::Apply(int input) {
    return input;
}
}

int run_manifest() {
    return simulator::ManifestStartup::Apply(1);
}
