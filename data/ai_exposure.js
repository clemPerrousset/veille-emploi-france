// Estimation d'exposition à l'IA générative par catégorie Indeed (0 = très peu, 100 = très exposé).
// ATTENTION : ce sont des ESTIMATIONS qualitatives (jugement éditorial), inspirées des travaux publics
// (OIT/ILO 2025, Anthropic Economic Index, Eloundou et al. 2023) mais PAS une reprise de leurs scores.
// À remplacer par un vrai rapprochement ROME/ISCO/SOC dans une prochaine version.
// "groupe" = regroupement en secteurs (moyenne simple des catégories, pas pondérée par l'emploi).
window.AI = {
  "Software Development": { fr: "Développement logiciel", groupe: "Tech", expo: 80, tech: true },
  "IT Systems & Solutions": { fr: "Systèmes & solutions IT", groupe: "Tech", expo: 70, tech: true },
  "IT Infrastructure, Operations & Support": { fr: "Infra, exploitation & support IT", groupe: "Tech", expo: 55, tech: true },

  "Accounting": { fr: "Comptabilité", groupe: "Finance & administratif", expo: 85 },
  "Administrative Assistance": { fr: "Assistanat administratif", groupe: "Finance & administratif", expo: 85 },
  "Banking & Finance": { fr: "Banque & finance", groupe: "Finance & administratif", expo: 75 },
  "Insurance": { fr: "Assurance", groupe: "Finance & administratif", expo: 75 },
  "Legal": { fr: "Juridique", groupe: "Finance & administratif", expo: 75 },
  "Human Resources": { fr: "Ressources humaines", groupe: "Finance & administratif", expo: 65 },

  "Customer Service": { fr: "Service client", groupe: "Commerce & relation client", expo: 85 },
  "Marketing": { fr: "Marketing", groupe: "Commerce & relation client", expo: 75 },
  "Sales": { fr: "Vente (commerciaux)", groupe: "Commerce & relation client", expo: 45 },
  "Retail": { fr: "Commerce de détail", groupe: "Commerce & relation client", expo: 30 },
  "Real Estate": { fr: "Immobilier", groupe: "Commerce & relation client", expo: 50 },

  "Media & Communications": { fr: "Médias & communication", groupe: "Éducation, médias & culture", expo: 80 },
  "Arts & Entertainment": { fr: "Arts & spectacle", groupe: "Éducation, médias & culture", expo: 50 },
  "Education & Instruction": { fr: "Éducation & formation", groupe: "Éducation, médias & culture", expo: 45 },

  "Management": { fr: "Management", groupe: "Gestion de projet & management", expo: 45 },
  "Project Management": { fr: "Gestion de projet", groupe: "Gestion de projet & management", expo: 60 },

  "Nursing": { fr: "Soins infirmiers", groupe: "Santé & social", expo: 12 },
  "Physicians & Surgeons": { fr: "Médecins & chirurgiens", groupe: "Santé & social", expo: 30 },
  "Pharmacy": { fr: "Pharmacie", groupe: "Santé & social", expo: 30 },
  "Childcare": { fr: "Petite enfance", groupe: "Santé & social", expo: 10 },

  "Industrial Engineering": { fr: "Ingénierie industrielle", groupe: "Industrie, BTP & maintenance", expo: 45 },
  "Production & Manufacturing": { fr: "Production & fabrication", groupe: "Industrie, BTP & maintenance", expo: 25 },
  "Construction": { fr: "BTP", groupe: "Industrie, BTP & maintenance", expo: 10 },
  "Installation & Maintenance": { fr: "Installation & maintenance", groupe: "Industrie, BTP & maintenance", expo: 12 },
  "Agriculture & Forestry": { fr: "Agriculture & forêt", groupe: "Industrie, BTP & maintenance", expo: 10 },

  "Logistic Support": { fr: "Support logistique", groupe: "Logistique & sécurité", expo: 35 },
  "Loading & Stocking": { fr: "Manutention & stockage", groupe: "Logistique & sécurité", expo: 15 },
  "Security & Public Safety": { fr: "Sécurité", groupe: "Logistique & sécurité", expo: 15 },

  "Food Preparation & Service": { fr: "Restauration", groupe: "Services à la personne", expo: 5 },
  "Cleaning & Sanitation": { fr: "Nettoyage", groupe: "Services à la personne", expo: 5 },
  "Beauty & Wellness": { fr: "Beauté & bien-être", groupe: "Services à la personne", expo: 8 }
};
