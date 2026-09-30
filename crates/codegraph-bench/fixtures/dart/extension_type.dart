extension type MetersT(double value) {
  double get km => value / 1000;

  void report() {
    print(km);
  }
}

class Widget {
  double get half => 1.0;
}
