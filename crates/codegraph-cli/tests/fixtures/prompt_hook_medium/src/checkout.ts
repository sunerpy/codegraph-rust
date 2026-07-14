export class CheckoutStateMachine {
  private current = "cart";

  transition(next: string): string {
    this.current = next;
    return this.current;
  }
}

export class CheckoutService {
  private machine = new CheckoutStateMachine();

  begin(): string {
    return this.machine.transition("payment");
  }
}

export class CheckoutController {
  private service = new CheckoutService();

  handle(): string {
    return this.service.begin();
  }
}
