import { createFileRoute } from '@tanstack/react-router';

export const Route = createFileRoute('/posts')({
  component: Posts,
});

function Posts() {
  return <ul />;
}
