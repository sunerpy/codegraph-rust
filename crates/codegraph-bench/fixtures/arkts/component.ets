import { Foo } from '../foo';

function helper(): void {}

function driver(): void {
  helper()
}

@Component
struct MyView {
  build() {
    helper()
  }
}

class Model {}
