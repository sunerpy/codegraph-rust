import { redirect } from '@sveltejs/kit';

export function load({ params }) {
  if (!params.slug) {
    redirect(307, '/');
  }
  return { slug: params.slug };
}
