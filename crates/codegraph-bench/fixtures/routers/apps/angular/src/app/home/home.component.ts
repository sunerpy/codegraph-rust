import { Component } from '@angular/core';
import { Router } from '@angular/router';

@Component({
  selector: 'app-home',
  template: `<button (click)="open()">{{ title() }}</button>`,
})
export class HomeComponent {
  constructor(private router: Router) {}

  title(): string {
    return 'home';
  }

  open(): void {
    this.router.navigate(['/lazy']);
  }
}
