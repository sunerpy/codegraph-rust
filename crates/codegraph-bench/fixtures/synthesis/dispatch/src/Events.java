package app;

import org.springframework.context.ApplicationEventPublisher;
import org.springframework.context.event.EventListener;

public class Events {
    private final ApplicationEventPublisher publisher;

    public Events(ApplicationEventPublisher publisher) {
        this.publisher = publisher;
    }

    public void register(String user) {
        publisher.publishEvent(new UserRegistered(user));
    }

    @EventListener
    public void onRegistered(UserRegistered event) {
        welcome(event.user);
    }

    private void welcome(String user) {
    }

    static class UserRegistered {
        final String user;

        UserRegistered(String user) {
            this.user = user;
        }
    }
}
