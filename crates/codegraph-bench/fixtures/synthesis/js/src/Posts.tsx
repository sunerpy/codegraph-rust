import { useGetPostsQuery, useAddPostMutation } from './api';

export function Posts() {
  const { data } = useGetPostsQuery();
  const [addPost] = useAddPostMutation();
  return data ? addPost : null;
}
