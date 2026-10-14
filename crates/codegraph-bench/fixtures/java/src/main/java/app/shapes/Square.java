package app.shapes;

import static app.util.MathUtil.square;

public class Square extends Base implements Shape {
    private final int side;

    public Square(int side) { this.side = side; }

    @Override
    public int area() { return square(side); }

    public static int pick(int x) { return x; }
    public static int pick(String s) { return s.length(); }
}

class Base {
    void start() { }
}
