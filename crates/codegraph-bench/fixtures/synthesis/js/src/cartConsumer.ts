import { useCartStore } from './store';

export function addToCart(item: string) {
  const cart = useCartStore();
  cart.add(item);
}
