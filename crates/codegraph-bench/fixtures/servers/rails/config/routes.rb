Rails.application.routes.draw do
  resources :users, only: %i[index show]
  namespace :admin do
    resources :reports, except: [:destroy]
  end
  get "health", to: "health#show"
end
