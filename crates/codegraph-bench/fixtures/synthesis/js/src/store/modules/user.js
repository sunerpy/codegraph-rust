import { persistToken } from '../auth';

export default {
  namespaced: true,
  state: () => ({ token: '' }),
  mutations: {
    SET_TOKEN(state, token) {
      state.token = token;
    },
  },
  actions: {
    login({ commit }, token) {
      persistToken(token);
      commit('SET_TOKEN', token);
    },
  },
};
