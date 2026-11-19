class WelcomeWorker
  include Sidekiq::Worker

  def perform(user_id)
    user_id
  end
end
