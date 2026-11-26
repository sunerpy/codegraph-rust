export function signIn(store, token) {
  store.dispatch('user/login', token);
}
