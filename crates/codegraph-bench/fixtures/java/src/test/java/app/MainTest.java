package app;

import static org.mockito.Mockito.mock;
import static org.mockito.Mockito.verify;

import app.a.Field;
import java.lang.reflect.Method;

public class MainTest {
    static class Field {
        String id() { return "nested"; }
    }

    void checks() throws Exception {
        Object service = mock(Object.class);
        verify(service);
        app.a.Field real = new app.a.Field();
        real.name();
        Method m = Object.class.getMethod("toString");
    }
}
