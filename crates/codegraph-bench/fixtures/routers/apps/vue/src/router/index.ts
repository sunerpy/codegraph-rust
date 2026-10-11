import { createRouter, createWebHistory } from 'vue-router';
import Home from '../views/Home.vue';

export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', name: 'home', component: Home },
    {
      path: '/about',
      component: () => import('../views/About.vue'),
      children: [{ path: 'team', component: () => import('../views/About.vue') }],
    },
  ],
});

export function goAbout() {
  return router.push('/about');
}
