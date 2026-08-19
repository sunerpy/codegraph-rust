export class Mailer { send(m: string): void { console.log(m); } }
export class Outbox {
  private mailer = new Mailer();
  send(m: string): void { this.mailer.send(m); }
}
export class Relay {
  private mailer = new Mailer();
  forward(m: string): void { this.mailer.send(m); }
}
