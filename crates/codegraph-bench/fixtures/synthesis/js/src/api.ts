import { createApi, fetchBaseQuery } from '@reduxjs/toolkit/query/react';

async function loadPosts() {
  return [];
}

export const postsApi = createApi({
  reducerPath: 'posts',
  baseQuery: fetchBaseQuery({ baseUrl: '/' }),
  endpoints: (build) => ({
    getPosts: build.query({
      queryFn: async () => ({ data: await loadPosts() }),
    }),
    addPost: build.mutation({
      query: (body) => ({ url: 'posts', method: 'POST', body }),
    }),
  }),
});

export const { useGetPostsQuery, useLazyGetPostsQuery, useAddPostMutation } = postsApi;
