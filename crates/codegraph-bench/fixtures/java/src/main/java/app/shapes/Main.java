package app.shapes;

import java.util.ArrayList;
import java.util.List;

public class Main {
    enum Op {
        ADD {
            int apply(int a, int b) { return combine(a, b); }
        },
        SUB {
            int apply(int a, int b) { return a - b; }
        };

        abstract int apply(int a, int b);

        static int combine(int a, int b) { return a + b; }
    }

    Square build(Square p) {
        Square s = new Square(2);
        List<Square> all = new ArrayList<>();
        all.add(s);
        Runnable r = new Runnable() {
            @Override
            public void run() { helper(); }
        };
        r.run();
        return s;
    }

    int picks() { return Square.pick(1) + Square.pick("x"); }

    void helper() { }
}
